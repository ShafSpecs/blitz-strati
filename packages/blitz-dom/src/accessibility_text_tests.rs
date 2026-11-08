use super::*;

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
