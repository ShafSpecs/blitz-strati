use super::*;

#[test]
fn textarea_text_runs_follow_live_value_without_initial_dom_text() {
    use crate::{DocumentConfig, EventDriver, NoopEventHandler, qual_name};
    use accesskit::{ActionData, ActionRequest};
    use blitz_traits::shell::{ColorScheme, Viewport};

    let mut doc = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
        ..Default::default()
    });
    let root = doc.root_node().id;
    let mut m = doc.mutate();
    let html = m.create_element(qual_name!("html", html), vec![]);
    let body = m.create_element(qual_name!("body", html), vec![]);
    let textarea = m.create_element(qual_name!("textarea", html), vec![]);
    let initial = m.create_text_node("init");
    m.append_children(textarea, &[initial]);
    m.append_children(body, &[textarea]);
    m.append_children(html, &[body]);
    m.append_children(root, &[html]);
    drop(m);
    doc.resolve(0.0);

    let mut observed = Vec::new();
    for value in ["init", "edited🙂", ""] {
        if value != "init" {
            let request = ActionRequest {
                action: Action::SetValue,
                target_tree: TreeId::ROOT,
                target_node: NodeId(textarea.as_u64()),
                data: Some(ActionData::Value(value.into())),
            };
            assert!(EventDriver::new(&mut doc, NoopEventHandler).handle_accessibility_action(&request));
        }
        let tree = doc.build_accessibility_tree();
        let input = &tree.nodes.iter().find(|(id, _)| id.0 == textarea.as_u64()).unwrap().1;
        assert_eq!(input.value(), Some(value));
        let editor = &doc.get_node(textarea).unwrap().element_data().unwrap().text_input_data().unwrap().editor;
        let selection = input.text_selection().expect("text input exposes its real caret");
        assert_eq!(selection.focus.node, input.children()[0]);
        assert_eq!(selection.anchor.node, input.children()[0]);
        assert_eq!(selection.focus.character_index, value[..editor.raw_selection().focus().index()].graphemes(true).count());
        assert_eq!(selection.anchor.character_index, value[..editor.raw_selection().anchor().index()].graphemes(true).count());
        let runs: Vec<_> = input.children().iter().map(|child| {
            let run = &tree.nodes.iter().find(|(id, _)| id == child).unwrap().1;
            assert_eq!(run.role(), Role::TextRun);
            run.value().unwrap().to_owned()
        }).collect();
        observed.push(runs);
    }
    assert_eq!(observed, vec![vec!["init"], vec!["edited🙂"], vec![""]]);
}

#[test]
fn input_text_runs_cover_unicode_empty_and_masked_values() {
    for (role, value) in [
        (Role::TextInput, "café🙂"),
        (Role::TextInput, ""),
        (Role::PasswordInput, "••••"),
    ] {
        let mut input = AccessKitNode::new(role);
        input.set_value(value);
        let mut nodes = vec![(NodeId(1), input)];
        add_input_text_runs(&mut nodes);
        assert_eq!(nodes.len(), 2);
        let (run_id, run) = &nodes[1];
        assert_eq!(nodes[0].1.children(), &[*run_id]);
        assert_eq!(run.role(), Role::TextRun);
        assert_eq!(run.value(), Some(value));
        assert_eq!(run.character_lengths().iter().map(|n| *n as usize).sum::<usize>(), value.len());
    }
}

#[test]
fn synthetic_runs_do_not_collide_with_existing_nodes_or_window() {
    let mut input = AccessKitNode::new(Role::TextInput);
    input.set_value("text");
    let mut nodes = vec![
        (NodeId(1), input),
        (NodeId(u64::MAX - 1), AccessKitNode::new(Role::Button)),
    ];
    add_input_text_runs(&mut nodes);
    assert_eq!(nodes[0].1.children(), &[NodeId(u64::MAX - 2)]);
    let mut unique = std::collections::HashSet::new();
    assert!(nodes.iter().all(|(id, _)| unique.insert(*id)));
}

#[test]
fn controls_without_text_values_do_not_get_text_runs() {
    let mut nodes = vec![(NodeId(1), AccessKitNode::new(Role::Button))];
    add_input_text_runs(&mut nodes);
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].1.children().is_empty());
}

#[test]
fn character_lengths_preserve_graphemes_and_long_clusters() {
    assert_eq!(text_character_lengths("e\u{301}🙂"), vec![3, 4]);
    assert!(text_character_lengths("").is_empty());
    let long_cluster = "e".to_owned() + &"\u{301}".repeat(200);
    let lengths = text_character_lengths(&long_cluster);
    assert_eq!(lengths.iter().map(|n| *n as usize).sum::<usize>(), long_cluster.len());
    assert!(lengths.iter().all(|n| *n > 0));
}
