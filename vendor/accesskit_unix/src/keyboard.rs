use futures_lite::{StreamExt, future};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    future::Future,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use zbus::{
    Connection, MatchRule, MessageStream,
    zvariant::{OwnedObjectPath, Type},
};

pub(crate) static LISTENERS: AtomicBool = AtomicBool::new(false);
pub(crate) const KEY_TIMEOUT: Duration = Duration::from_millis(50);
const REGISTRY: &str = "org.a11y.atspi.Registry";
const PATH: &str = "/org/a11y/atspi/registry/deviceeventcontroller";
const INTERFACE: &str = "org.a11y.atspi.DeviceEventController";
static PENDING: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| Arc::new(AtomicBool::new(false)));

pub(crate) struct KeyPermit(Arc<AtomicBool>);

impl Drop for KeyPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// A native key event. Callers must apply their protected-input policy before forwarding.
/// Forwarded command exceptions retain their identifiers, hardware codes, and strings.
#[derive(Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct KeyEvent {
    pub event_type: u32,
    pub id: i32,
    pub hw_code: i16,
    pub modifiers: i16,
    pub timestamp: i32,
    pub event_string: String,
    pub is_text: bool,
}

pub(crate) async fn bounded<T>(
    duration: Duration,
    operation: impl Future<Output = T>,
) -> Option<T> {
    future::race(async { Some(operation.await) }, async {
        #[cfg(not(feature = "tokio"))]
        async_io::Timer::after(duration).await;
        #[cfg(feature = "tokio")]
        tokio::time::sleep(duration).await;
        None
    })
    .await
}

pub(crate) fn request(
    event: KeyEvent,
    send: impl FnOnce(KeyEvent, Instant, mpsc::Sender<bool>, KeyPermit),
) -> bool {
    request_with(&PENDING, event, send)
}

fn request_with(
    pending: &Arc<AtomicBool>,
    event: KeyEvent,
    send: impl FnOnce(KeyEvent, Instant, mpsc::Sender<bool>, KeyPermit),
) -> bool {
    if pending
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    let permit = KeyPermit(Arc::clone(pending));
    let deadline = Instant::now() + KEY_TIMEOUT;
    let (tx, rx) = mpsc::channel();
    send(event, deadline, tx, permit);
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap_or(false)
}

pub(crate) async fn forward(conn: &Connection, event: KeyEvent, deadline: Instant) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() || !LISTENERS.load(Ordering::Acquire) {
        return false;
    }
    bounded(remaining, async {
        let reply = conn
            .call_method(
                Some(REGISTRY),
                PATH,
                Some(INTERFACE),
                "NotifyListenersSync",
                &(event,),
            )
            .await?;
        reply.body().deserialize::<bool>()
    })
    .await
    .and_then(Result::ok)
    .unwrap_or(false)
}

#[derive(Clone, Deserialize, Type, PartialEq, Eq, Hash)]
struct Listener(
    String,
    OwnedObjectPath,
    u32,
    u32,
    Vec<(i32, i32, String, i32)>,
    u32,
    (bool, bool, bool),
);

pub(crate) async fn track_listeners(conn: Connection) -> zbus::Result<()> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            LISTENERS.store(false, Ordering::Release);
        }
    }
    let _reset = Reset;
    LISTENERS.store(false, Ordering::Release);
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(REGISTRY)?
        .path(PATH)?
        .interface("org.a11y.atspi.DeviceEventListener")?
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &conn, Some(512)).await?;
    let mut listeners = HashSet::new();
    if let Some(Ok(reply)) = bounded(
        Duration::from_millis(500),
        conn.call_method(
            Some(REGISTRY),
            PATH,
            Some(INTERFACE),
            "GetKeystrokeListeners",
            &(),
        ),
    )
    .await
    {
        if let Ok((snapshot,)) = reply.body().deserialize::<(Vec<Listener>,)>() {
            listeners.extend(snapshot);
        }
    }
    update_listeners(&listeners);
    while let Some(Ok(message)) = stream.next().await {
        let member = message.header().member().map(|name| name.to_string());
        if let Ok((listener,)) = message.body().deserialize::<(Listener,)>() {
            match member.as_deref() {
                Some("KeystrokeListenerRegistered") => {
                    listeners.insert(listener);
                }
                Some("KeystrokeListenerDeregistered") => {
                    deregister_listener(&mut listeners, &listener);
                }
                _ => continue,
            }
            update_listeners(&listeners);
        }
    }
    Ok(())
}

fn update_listeners(listeners: &HashSet<Listener>) {
    LISTENERS.store(has_local_listeners(listeners), Ordering::Release);
}

fn has_local_listeners(listeners: &HashSet<Listener>) -> bool {
    listeners.iter().any(|listener| !listener.6.2)
}

fn deregister_listener(listeners: &mut HashSet<Listener>, removed: &Listener) {
    listeners.retain(|listener| listener.0 != removed.0 || listener.1 != removed.1);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> KeyEvent {
        KeyEvent {
            event_type: 0,
            id: 97,
            hw_code: 38,
            modifiers: 4,
            timestamp: 123,
            event_string: "a".into(),
            is_text: true,
        }
    }
    #[test]
    fn wire_payload() {
        use zbus::zvariant::{LE, serialized::Context, to_bytes};
        let event = event();
        assert_eq!(KeyEvent::SIGNATURE.to_string(), "(uinnisb)");
        let bytes = to_bytes(Context::new_dbus(LE, 0), &event).unwrap();
        let (decoded, _): (KeyEvent, _) = bytes.deserialize().unwrap();
        assert!(decoded == event);
        let message = zbus::Message::method_call(PATH, "NotifyListenersSync")
            .unwrap()
            .build(&(event,))
            .unwrap();
        assert!(
            message
                .data()
                .bytes()
                .windows(b"\x09(uinnisb)\0".len())
                .any(|window| window == b"\x09(uinnisb)\0")
        );
    }
    #[test]
    fn listener_protocol_and_fast_path() {
        assert_eq!(Listener::SIGNATURE.to_string(), "(souua(iisi)u(bbb))");
        let listener = Listener(
            ":1.1".into(),
            OwnedObjectPath::try_from("/listener").unwrap(),
            1,
            3,
            vec![],
            0,
            (true, true, false),
        );
        let mut listeners = HashSet::new();
        assert!(!has_local_listeners(&listeners));
        listeners.insert(listener.clone());
        assert!(has_local_listeners(&listeners));
        let mut removed = listener.clone();
        removed.6 = (false, false, false);
        deregister_listener(&mut listeners, &removed);
        assert!(listeners.is_empty());
        let mut global = listener;
        global.6.2 = true;
        listeners.insert(global);
        assert!(!has_local_listeners(&listeners));
    }
    #[test]
    fn deregister_removes_all_endpoint_registrations_and_preserves_other_endpoints() {
        let first = Listener(
            ":1.1".into(),
            OwnedObjectPath::try_from("/listener").unwrap(),
            1,
            1,
            vec![],
            0,
            (true, true, false),
        );
        let mut second = first.clone();
        second.3 = 2;
        second.5 = 4;
        second.4 = vec![(38, 97, "a".into(), 0)];
        let mut other = first.clone();
        other.1 = OwnedObjectPath::try_from("/other").unwrap();
        let mut listeners = HashSet::from([first.clone(), second, other.clone()]);
        let mut removed = first;
        removed.6 = (false, false, false);
        deregister_listener(&mut listeners, &removed);
        assert!(listeners == HashSet::from([other]));
        assert!(has_local_listeners(&listeners));
    }
    #[test]
    fn unavailable_consumed_and_timeout() {
        assert!(!request(event(), |_, _, _, _| {}));
        assert!(request(event(), |_, _, tx, _permit| {
            tx.send(true).unwrap();
        }));
        assert!(!request(event(), |_, _, tx, _permit| {
            tx.send(false).unwrap();
        }));
        let started = Instant::now();
        let keep = std::sync::Mutex::new(None);
        assert!(!request(event(), |_, _, tx, _permit| {
            *keep.lock().unwrap() = Some(tx);
        }));
        assert!(started.elapsed() >= KEY_TIMEOUT);
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(keep.lock().unwrap().take().unwrap().send(true).is_err());
    }

    #[test]
    fn stalled_queue_retains_permit_until_drained_then_recovers() {
        let pending = Arc::new(AtomicBool::new(false));
        let mut queued = None;
        let started = Instant::now();
        assert!(!request_with(
            &pending,
            event(),
            |event, deadline, reply, permit| {
                queued = Some(crate::adapter::Message::ForwardKey {
                    event,
                    deadline,
                    reply,
                    permit,
                });
            }
        ));
        assert!(started.elapsed() >= KEY_TIMEOUT);
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(pending.load(Ordering::Acquire));
        let started = Instant::now();
        for _ in 0..100 {
            assert!(!request_with(&pending, event(), |_, _, _, _| panic!(
                "queued a second key"
            )));
        }
        assert!(started.elapsed() < KEY_TIMEOUT);
        drop(queued.take());
        assert!(!pending.load(Ordering::Acquire));
        assert!(request_with(&pending, event(), |_, _, reply, _permit| {
            reply.send(true).unwrap();
        }));
        assert!(!pending.load(Ordering::Acquire));
    }

    #[test]
    fn stalled_bus_future_is_cancelled() {
        crate::util::block_on(async {
            assert!(
                bounded(KEY_TIMEOUT, std::future::ready(true))
                    .await
                    .unwrap()
            );
            assert!(
                bounded(KEY_TIMEOUT, std::future::pending::<bool>())
                    .await
                    .is_none()
            );
        });
    }
}
