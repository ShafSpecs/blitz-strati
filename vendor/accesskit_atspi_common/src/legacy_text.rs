use accesskit_consumer::NodeRef;
use unicode_segmentation::UnicodeSegmentation;

use crate::Error;

pub(crate) fn get_text_offsets(start: i32, end: i32, count: usize) -> Result<(i32, i32), Error> {
    let count = i32::try_from(count).map_err(|_| Error::TooManyCharacters)?;
    if start < 0 || end < -1 || (end != -1 && end < start) {
        return Err(Error::IndexOutOfRange);
    }
    Ok((
        start.min(count),
        if end == -1 { count } else { end.min(count) },
    ))
}

pub(crate) fn text_at_boundary(
    node: &NodeRef,
    offset: i32,
    boundary: u32,
    relative: i32,
) -> Result<(String, i32, i32), Error> {
    let text = node.document_range().text();
    let mut boundaries = text_boundaries(&text, boundary)?;
    if matches!(boundary, 5 | 6) {
        let mut index = 0;
        while let Some(line) = node.line_range_from_index(index) {
            let position = if boundary == 5 {
                line.start()
            } else {
                line.end()
            };
            boundaries.push(position.to_global_usv_index());
            if line.end().is_document_end() {
                break;
            }
            index += 1;
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let count = text.chars().count();
    let terminal_boundary = match boundary {
        2 => text
            .unicode_word_indices()
            .last()
            .is_some_and(|(start, word)| start + word.len() == text.len()),
        4 => !text.ends_with(char::is_whitespace),
        5 => text.ends_with('\n'),
        _ => false,
    };
    let containing_offset =
        if offset >= 0 && offset as usize == count && !terminal_boundary && boundary != 0 {
            offset.saturating_sub(1).max(0)
        } else {
            offset
        };
    let (start, end) = if boundary == 0 {
        character_range(&boundaries, offset, relative, count)?
    } else {
        boundary_range(&boundaries, containing_offset, relative, count)?
    };
    let start = i32::try_from(start).map_err(|_| Error::TooManyCharacters)?;
    let end = i32::try_from(end).map_err(|_| Error::TooManyCharacters)?;
    let value = text
        .chars()
        .skip(start as usize)
        .take((end - start) as usize)
        .collect();
    Ok((value, start, end))
}

fn text_boundaries(text: &str, boundary: u32) -> Result<Vec<usize>, Error> {
    let count = text.chars().count();
    let mut offsets = vec![0, count];
    let mut characters = text.char_indices().peekable();
    let mut scalar = 0;
    let mut scalar_index = |byte: usize| {
        while characters.peek().is_some_and(|(index, _)| *index < byte) {
            characters.next();
            scalar += 1;
        }
        scalar
    };
    match boundary {
        0 => offsets.extend(
            text.grapheme_indices(true)
                .map(|(byte, _)| scalar_index(byte)),
        ),
        1 | 2 => {
            for (byte, word) in text.unicode_word_indices() {
                offsets.push(scalar_index(if boundary == 1 {
                    byte
                } else {
                    byte + word.len()
                }));
            }
        }
        3 | 4 => {
            for (byte, sentence) in text.split_sentence_bound_indices() {
                let trimmed = sentence.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let start = byte + sentence.len() - sentence.trim_start().len();
                offsets.push(scalar_index(if boundary == 3 {
                    start
                } else {
                    start + trimmed.len()
                }));
            }
        }
        5 | 6 => {
            for (index, character) in text.chars().enumerate() {
                if character == '\n' {
                    offsets.push(if boundary == 5 { index + 1 } else { index });
                }
            }
        }
        _ => return Err(Error::UnsupportedTextGranularity),
    }
    offsets.sort_unstable();
    offsets.dedup();
    Ok(offsets)
}

fn boundary_range(
    boundaries: &[usize],
    offset: i32,
    relative: i32,
    count: usize,
) -> Result<(usize, usize), Error> {
    let offset = usize::try_from(offset).map_err(|_| Error::IndexOutOfRange)?;
    if offset > count {
        return Err(Error::IndexOutOfRange);
    }
    let index = boundaries
        .partition_point(|b| *b <= offset)
        .saturating_sub(1) as i64
        + i64::from(relative);
    if index < 0 {
        return Ok((0, 0));
    }
    let index = index as usize;
    Ok((
        boundaries.get(index).copied().unwrap_or(count),
        boundaries.get(index + 1).copied().unwrap_or(count),
    ))
}

fn character_range(
    boundaries: &[usize],
    offset: i32,
    relative: i32,
    count: usize,
) -> Result<(usize, usize), Error> {
    let offset = usize::try_from(offset).map_err(|_| Error::IndexOutOfRange)?;
    if offset > count {
        return Err(Error::IndexOutOfRange);
    }
    let next = boundaries.partition_point(|b| *b <= offset);
    let previous = boundaries.partition_point(|b| *b < offset);
    match relative {
        -1 => Ok((
            boundaries
                .get(previous.saturating_sub(1))
                .copied()
                .unwrap_or(0),
            offset,
        )),
        0 => Ok((offset, boundaries.get(next).copied().unwrap_or(count))),
        _ => Ok((
            boundaries.get(next).copied().unwrap_or(count),
            boundaries.get(next + 1).copied().unwrap_or(count),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_unicode_boundaries_scale_to_long_text() {
        let sample = "é e\u{301}. 🙂 Hi!\n";
        let segments = [
            vec![0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11],
            vec![0, 2, 8],
            vec![1, 4, 10],
            vec![0, 6],
            vec![5, 11],
            vec![12],
            vec![11],
        ];
        for repetitions in [1, 5000] {
            let text = sample.repeat(repetitions);
            let count = 12 * repetitions;
            for (boundary, segment) in segments.iter().enumerate() {
                let mut expected = vec![0, count];
                for index in 0..repetitions {
                    expected.extend(segment.iter().map(|offset| 12 * index + offset));
                }
                expected.sort_unstable();
                expected.dedup();
                assert_eq!(
                    text_boundaries(&text, boundary as u32).unwrap(),
                    expected,
                    "boundary {boundary}, repetitions {repetitions}"
                );
            }
        }
    }

    #[test]
    fn atk_document_end_keeps_the_last_start_delimited_segment() {
        use accesskit::{Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate};
        let mut input = Node::new(Role::TextInput);
        input.set_children([NodeId(1)]);
        let mut run = Node::new(Role::TextRun);
        run.set_value("city");
        run.set_character_lengths([1, 1, 1, 1]);
        let tree = accesskit_consumer::Tree::new(
            TreeUpdate {
                nodes: vec![(NodeId(0), input), (NodeId(1), run)],
                tree: Some(TreeInfo::new(NodeId(0))),
                tree_id: TreeId::ROOT,
                focus: NodeId(0),
            },
            true,
        );
        let node = tree.state().root();
        for boundary in [1, 3, 5, 6] {
            assert_eq!(
                text_at_boundary(&node, 4, boundary, 0).unwrap(),
                ("city".into(), 0, 4)
            );
            assert_eq!(
                text_at_boundary(&node, 4, boundary, -1).unwrap(),
                ("".into(), 0, 0)
            );
            assert_eq!(
                text_at_boundary(&node, 4, boundary, 1).unwrap(),
                ("".into(), 4, 4)
            );
        }
        for boundary in [0, 2, 4] {
            assert_eq!(
                text_at_boundary(&node, 4, boundary, 0).unwrap(),
                ("".into(), 4, 4)
            );
        }
        assert!(text_at_boundary(&node, -1, 5, 0).is_err());
        assert!(text_at_boundary(&node, 5, 5, 0).is_err());
        assert!(text_at_boundary(&node, 0, 7, 0).is_err());
    }

    #[test]
    fn gtk_get_text_clamps_empty_and_end_but_rejects_negative_or_reversed_ranges() {
        assert_eq!(get_text_offsets(0, 1, 0).unwrap(), (0, 0));
        assert_eq!(get_text_offsets(0, -1, 4).unwrap(), (0, 4));
        assert_eq!(get_text_offsets(0, 100, 4).unwrap(), (0, 4));
        assert_eq!(get_text_offsets(100, 101, 4).unwrap(), (4, 4));
        assert!(get_text_offsets(-1, 4, 4).is_err());
        assert!(get_text_offsets(0, -2, 4).is_err());
        assert!(get_text_offsets(3, 2, 4).is_err());
    }

    #[test]
    fn atk_start_and_end_boundaries_preserve_delimiters() {
        let text = "  one two.  Three! ";
        for (boundary, expected) in [
            (
                0,
                vec![
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
                ],
            ),
            (1, vec![0, 2, 6, 12, 19]),
            (2, vec![0, 5, 9, 17, 19]),
            (3, vec![0, 2, 12, 19]),
            (4, vec![0, 10, 18, 19]),
            (5, vec![0, 19]),
            (6, vec![0, 19]),
        ] {
            let offsets = text_boundaries(text, boundary).unwrap();
            assert_eq!(offsets, expected, "boundary {boundary}");
            for (index, pair) in offsets.windows(2).enumerate() {
                assert_eq!(
                    boundary_range(&offsets, pair[0] as i32, 0, 19).unwrap(),
                    (pair[0], pair[1])
                );
                let before = if index == 0 {
                    (0, 0)
                } else {
                    (offsets[index - 1], pair[0])
                };
                assert_eq!(
                    boundary_range(&offsets, pair[0] as i32, -1, 19).unwrap(),
                    before
                );
                let after = (pair[1], offsets.get(index + 2).copied().unwrap_or(19));
                assert_eq!(
                    boundary_range(&offsets, pair[0] as i32, 1, 19).unwrap(),
                    after
                );
            }
        }
    }

    #[test]
    fn empty_unicode_newline_and_invalid_boundaries() {
        for boundary in 0..=6 {
            let offsets = text_boundaries("", boundary).unwrap();
            for relative in -1..=1 {
                assert_eq!(boundary_range(&offsets, 0, relative, 0).unwrap(), (0, 0));
            }
        }
        assert_eq!(text_boundaries("é🙂\nx", 5).unwrap(), vec![0, 3, 4]);
        assert_eq!(text_boundaries("é🙂\nx", 6).unwrap(), vec![0, 2, 4]);
        assert!(text_boundaries("x", 7).is_err());
        assert!(boundary_range(&[0, 1], -1, 0, 1).is_err());
        assert!(boundary_range(&[0, 1], 2, 0, 1).is_err());
        let clusters = text_boundaries("e\u{301}🙂", 0).unwrap();
        assert_eq!(character_range(&clusters, 0, 0, 3).unwrap(), (0, 2));
        assert_eq!(character_range(&clusters, 1, 0, 3).unwrap(), (1, 2));
        assert_eq!(character_range(&clusters, 1, -1, 3).unwrap(), (0, 1));
        assert_eq!(character_range(&clusters, 1, 1, 3).unwrap(), (2, 3));
    }
}
