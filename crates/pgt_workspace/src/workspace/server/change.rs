use pgt_suppressions::Suppressions;
use pgt_text_size::{TextLen, TextRange, TextSize};
use std::ops::{Add, Sub};

use crate::workspace::{ChangeFileParams, ChangeParams};

use super::{Document, document, statement_identifier::StatementId};

#[derive(Debug, PartialEq, Eq)]
pub enum StatementChange {
    Added(AddedStatement),
    Deleted(StatementId),
    Modified(ModifiedStatement),
}

#[derive(Debug, PartialEq, Eq)]
pub struct AddedStatement {
    pub stmt: StatementId,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ModifiedStatement {
    pub old_stmt: StatementId,
    pub old_stmt_text: String,

    pub new_stmt: StatementId,
    pub new_stmt_text: String,

    pub change_range: TextRange,
    pub change_text: String,
}

impl StatementChange {
    #[allow(dead_code)]
    pub fn statement(&self) -> &StatementId {
        match self {
            StatementChange::Added(stmt) => &stmt.stmt,
            StatementChange::Deleted(stmt) => stmt,
            StatementChange::Modified(changed) => &changed.new_stmt,
        }
    }
}

/// Returns all relevant details about the change and its effects on the current state of the document.
struct Affected {
    /// Full range of the change, including the range of all statements that intersect with the change
    affected_range: TextRange,
    /// All indices of affected statement positions
    affected_indices: Vec<usize>,
    /// The index of the first statement position before the change, if any
    prev_index: Option<usize>,
    /// The index of the first statement position after the change, if any
    next_index: Option<usize>,
    /// the full affected range includng the prev and next statement
    full_affected_range: TextRange,
}

impl Document {
    /// Applies a file change to the document and returns the affected statements
    pub fn apply_file_change(&mut self, change: &ChangeFileParams) -> Vec<StatementChange> {
        // cleanup all diagnostics with every change because we cannot guarantee that they are still valid
        // this is because we know their ranges only by finding slices within the content which is
        // very much not guaranteed to result in correct ranges
        self.diagnostics.clear();

        // when we recieive more than one change, we need to push back the changes based on the
        // total range of the previous ones. This is because the ranges are always related to the original state.
        // BUT: only for the statement range changes, not for the text changes
        // this is why we pass both varaints to apply_change
        let mut changes = Vec::new();

        let mut change_indices: Vec<usize> = (0..change.changes.len()).collect();
        change_indices.sort_by(|&a, &b| {
            match (change.changes[a].range, change.changes[b].range) {
                (Some(range_a), Some(range_b)) => range_b.start().cmp(&range_a.start()),
                (Some(_), None) => std::cmp::Ordering::Greater, // full changes will never be sent in a batch so this does not matter
                (None, Some(_)) => std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });

        // Sort changes by start position and process from last to first to avoid position invalidation
        for &idx in &change_indices {
            changes.extend(self.apply_change(&change.changes[idx]));
        }

        self.version = change.version;
        self.suppressions = Suppressions::from(self.content.as_str());

        changes
    }

    /// Helper method to drain all positions and return them as deleted statements
    fn drain_positions(&mut self) -> Vec<StatementChange> {
        self.positions
            .drain(..)
            .map(|(id, _)| StatementChange::Deleted(id))
            .collect()
    }

    /// Applies a change to the document and returns the affected statements
    ///
    /// Will always assume its a full change and reparse the whole document
    fn apply_full_change(&mut self, change: &ChangeParams) -> Vec<StatementChange> {
        let mut changes = Vec::new();

        changes.extend(self.drain_positions());

        self.content = change.apply_to_text(&self.content);

        let (ranges, diagnostics) = document::split_with_diagnostics(&self.content, None);

        self.diagnostics = diagnostics;

        // Do not add any statements if there is a fatal error
        if self.has_fatal_error() {
            return changes;
        }

        changes.extend(ranges.into_iter().map(|range| {
            let id = self.id_generator.next();
            let text = self.content[range].to_string();
            self.positions.push((id.clone(), range));

            StatementChange::Added(AddedStatement { stmt: id, text })
        }));

        changes
    }

    fn insert_statement(&mut self, range: TextRange) -> StatementId {
        let pos = self
            .positions
            .binary_search_by(|(_, r)| r.start().cmp(&range.start()))
            .unwrap_err();

        let new_id = self.id_generator.next();
        self.positions.insert(pos, (new_id.clone(), range));

        new_id
    }

    /// Returns all relevant details about the change and its effects on the current state of the document.
    /// - The affected range is the full range of the change, including the range of all statements that intersect with the change
    /// - All indices of affected statement positions
    /// - The index of the first statement position before the change, if any
    /// - The index of the first statement position after the change, if any
    /// - the full affected range includng the prev and next statement
    fn get_affected(
        &self,
        change_range: TextRange,
        content_size: TextSize,
        diff_size: TextSize,
        is_addition: bool,
    ) -> Affected {
        let mut start = change_range.start();
        let mut end = change_range.end().min(content_size);

        let is_trim = change_range.start() >= content_size;

        let mut affected_indices = Vec::new();
        let mut prev_index = None;
        let mut next_index = None;

        for (index, (_, pos_range)) in self.positions.iter().enumerate() {
            if pos_range.intersect(change_range).is_some() {
                affected_indices.push(index);
                start = start.min(pos_range.start());
                end = end.max(pos_range.end());
            } else if pos_range.end() <= change_range.start() {
                prev_index = Some(index);
            } else if pos_range.start() >= change_range.end() && next_index.is_none() {
                next_index = Some(index);
                break;
            }
        }

        if affected_indices.is_empty() && prev_index.is_none() {
            // if there is no prev_index and no intersection -> use 0
            start = 0.into();
        }

        if affected_indices.is_empty() && next_index.is_none() {
            // if there is no next_index and no intersection -> use content_size
            end = content_size;
        }

        let first_affected_stmt_start = prev_index
            .map(|i| self.positions[i].1.start())
            .unwrap_or(start);

        let mut last_affected_stmt_end = next_index
            .map(|i| self.positions[i].1.end())
            .unwrap_or_else(|| end);

        if is_addition {
            end = end.add(diff_size);
            last_affected_stmt_end = last_affected_stmt_end.add(diff_size);
        } else if !is_trim {
            end = end.sub(diff_size);
            last_affected_stmt_end = last_affected_stmt_end.sub(diff_size)
        };

        Affected {
            affected_range: {
                let end = end.min(content_size);
                TextRange::new(start.min(end), end)
            },
            affected_indices,
            prev_index,
            next_index,
            full_affected_range: TextRange::new(
                first_affected_stmt_start,
                last_affected_stmt_end
                    .min(content_size)
                    .max(first_affected_stmt_start),
            ),
        }
    }

    fn move_ranges(&mut self, offset: TextSize, diff_size: TextSize, is_addition: bool) {
        self.positions
            .iter_mut()
            .skip_while(|(_, r)| offset > r.start())
            .for_each(|(_, range)| {
                let new_range = if is_addition {
                    range.add(diff_size)
                } else {
                    range.sub(diff_size)
                };

                *range = new_range;
            });
    }

    /// Applies a single change to the document and returns the affected statements
    ///
    /// * `change`: The range-adjusted change to use for statement changes
    /// * `original_change`: The original change to use for text changes (yes, this is a bit confusing, and we might want to refactor this entire thing at some point.)
    fn apply_change(&mut self, change: &ChangeParams) -> Vec<StatementChange> {
        // if range is none, we have a full change
        if change.range.is_none() {
            // doesnt matter what change since range is null
            return self.apply_full_change(change);
        }

        // i spent a relatively large amount of time thinking about how to handle range changes
        // properly. there are quite a few edge cases to consider. I eventually skipped most of
        // them, because the complexity is not worth the return for now. we might want to revisit
        // this later though.

        let mut changed: Vec<StatementChange> = Vec::with_capacity(self.positions.len());

        let change_range = change.range.unwrap();
        let previous_content = self.content.clone();
        let new_content = change.apply_to_text(&self.content);

        // we first need to determine the affected range and all affected statements, as well as
        // the index of the prev and the next statement, if any. The full affected range is the
        // affected range expanded to the start of the previous statement and the end of the next
        let Affected {
            affected_range,
            affected_indices,
            prev_index,
            next_index,
            full_affected_range,
        } = self.get_affected(
            change_range,
            new_content.text_len(),
            change.diff_size(),
            change.is_addition(),
        );

        // if within a statement, we can modify it if the change results in also a single statement
        if affected_indices.len() == 1 {
            let changed_content = get_affected(&new_content, affected_range);

            let (new_ranges, diags) =
                document::split_with_diagnostics(changed_content, Some(affected_range.start()));

            self.diagnostics = diags;

            if self.has_fatal_error() {
                // cleanup all positions if there is a fatal error
                changed.extend(self.drain_positions());
                // still process text change
                self.content = new_content;
                return changed;
            }

            if new_ranges.len() == 1 {
                let affected_idx = affected_indices[0];
                let new_range = new_ranges[0].add(affected_range.start());
                let (old_id, old_range) = self.positions[affected_idx].clone();

                // move all statements after the affected range
                self.move_ranges(old_range.end(), change.diff_size(), change.is_addition());

                let new_id = self.id_generator.next();
                self.positions[affected_idx] = (new_id.clone(), new_range);

                changed.push(StatementChange::Modified(ModifiedStatement {
                    old_stmt: old_id.clone(),
                    old_stmt_text: previous_content[old_range].to_string(),

                    new_stmt: new_id,
                    new_stmt_text: changed_content[new_ranges[0]].to_string(),
                    // change must be relative to the statement
                    change_text: change.text.clone(),
                    // make sure we always have a valid range >= 0
                    change_range: change_range
                        .checked_sub(old_range.start())
                        .unwrap_or(change_range.sub(change_range.start())),
                }));

                self.content = new_content;

                return changed;
            }
        }

        // in any other case, parse the full affected range
        let changed_content = get_affected(&new_content, full_affected_range);

        let (new_ranges, diags) =
            document::split_with_diagnostics(changed_content, Some(full_affected_range.start()));

        self.diagnostics = diags;

        if self.has_fatal_error() {
            // cleanup all positions if there is a fatal error
            changed.extend(self.drain_positions());
            // still process text change
            self.content = new_content;
            return changed;
        }

        // delete and add new ones
        if let Some(next_index) = next_index {
            changed.push(StatementChange::Deleted(
                self.positions[next_index].0.clone(),
            ));
            self.positions.remove(next_index);
        }
        for idx in affected_indices.iter().rev() {
            changed.push(StatementChange::Deleted(self.positions[*idx].0.clone()));
            self.positions.remove(*idx);
        }
        if let Some(prev_index) = prev_index {
            changed.push(StatementChange::Deleted(
                self.positions[prev_index].0.clone(),
            ));
            self.positions.remove(prev_index);
        }

        new_ranges.iter().for_each(|range| {
            let actual_range = range.add(full_affected_range.start());
            let new_id = self.insert_statement(actual_range);
            changed.push(StatementChange::Added(AddedStatement {
                stmt: new_id,
                text: new_content[actual_range].to_string(),
            }));
        });

        // move all statements after the afffected range
        self.move_ranges(
            full_affected_range.end(),
            change.diff_size(),
            change.is_addition(),
        );

        self.content = new_content;

        changed
    }
}

impl ChangeParams {
    /// For lack of a better name, this returns the change in size of the text compared to the range
    pub fn change_size(&self) -> i64 {
        match self.range {
            Some(range) => {
                let range_length: usize = range.len().into();
                let text_length = self.text.chars().count();
                text_length as i64 - range_length as i64
            }
            None => i64::try_from(self.text.chars().count()).unwrap(),
        }
    }

    pub fn diff_size(&self) -> TextSize {
        match self.range {
            Some(range) => {
                let range_length: usize = range.len().into();
                let text_length = self.text.chars().count();
                let diff = (text_length as i64 - range_length as i64).abs();
                TextSize::from(u32::try_from(diff).unwrap())
            }
            None => TextSize::from(u32::try_from(self.text.chars().count()).unwrap()),
        }
    }

    pub fn is_addition(&self) -> bool {
        self.range.is_some() && self.text.len() > self.range.unwrap().len().into()
    }

    pub fn is_deletion(&self) -> bool {
        self.range.is_some() && self.text.len() < self.range.unwrap().len().into()
    }

    pub fn apply_to_text(&self, text: &str) -> String {
        if self.range.is_none() {
            return self.text.clone();
        }

        let range = self.range.unwrap();
        let start = usize::from(range.start());
        let end = usize::from(range.end());

        let mut new_text = String::new();
        new_text.push_str(&text[..start]);
        new_text.push_str(&self.text);
        if end < text.len() {
            new_text.push_str(&text[end..]);
        }

        new_text
    }
}

fn get_affected(content: &str, range: TextRange) -> &str {
    let start_byte = content
        .char_indices()
        .nth(usize::from(range.start()))
        .map(|(i, _)| i)
        .unwrap_or(content.len());

    let end_byte = content
        .char_indices()
        .nth(usize::from(range.end()))
        .map(|(i, _)| i)
        .unwrap_or(content.len());

    &content[start_byte..end_byte]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgt_text_size::TextRange;

    use crate::workspace::{ChangeFileParams, ChangeParams};

    use pgt_fs::PgTPath;

    impl Document {
        pub fn get_text(&self, idx: usize) -> String {
            self.content[self.positions[idx].1.start().into()..self.positions[idx].1.end().into()]
                .to_string()
        }
    }

    fn assert_document_integrity(d: &Document) {
        let ranges = pgt_statement_splitter::split(&d.content).ranges;

        assert!(
            ranges.len() == d.positions.len(),
            "should have the correct amount of positions"
        );

        assert!(
            ranges
                .iter()
                .all(|r| { d.positions.iter().any(|(_, stmt_range)| stmt_range == r) }),
            "all ranges should be in positions"
        );
    }

    #[test]
    fn comments_at_begin() {
        let path = PgTPath::new("test.sql");
        let input = "\nselect id from users;\n";

        let mut d = Document::new(input.to_string(), 0);

        let change1 = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "-".to_string(),
                range: Some(TextRange::new(0.into(), 0.into())),
            }],
        };

        let _changed1 = d.apply_file_change(&change1);

        assert_eq!(d.content, "-\nselect id from users;\n");
        assert_eq!(d.positions.len(), 2);

        let change2 = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: "-".to_string(),
                range: Some(TextRange::new(1.into(), 1.into())),
            }],
        };

        let _changed2 = d.apply_file_change(&change2);

        assert_eq!(d.content, "--\nselect id from users;\n");
        assert_eq!(d.positions.len(), 1);

        let change3 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(2.into(), 2.into())),
            }],
        };

        let _changed3 = d.apply_file_change(&change3);

        assert_eq!(d.content, "-- \nselect id from users;\n");
        assert_eq!(d.positions.len(), 1);

        let change4 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: "t".to_string(),
                range: Some(TextRange::new(3.into(), 3.into())),
            }],
        };

        let _changed4 = d.apply_file_change(&change4);

        assert_eq!(d.content, "-- t\nselect id from users;\n");
        assert_eq!(d.positions.len(), 1);

        assert_document_integrity(&d);
    }

    #[test]
    fn typing_comments() {
        let path = PgTPath::new("test.sql");
        let input = "select id from users;\n";

        let mut d = Document::new(input.to_string(), 0);

        let change1 = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "-".to_string(),
                range: Some(TextRange::new(22.into(), 23.into())),
            }],
        };

        let _changed1 = d.apply_file_change(&change1);

        assert_eq!(d.content, "select id from users;\n-");
        assert_eq!(d.positions.len(), 2);

        let change2 = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: "-".to_string(),
                range: Some(TextRange::new(23.into(), 24.into())),
            }],
        };

        let _changed2 = d.apply_file_change(&change2);

        assert_eq!(d.content, "select id from users;\n--");
        assert_eq!(d.positions.len(), 1);

        let change3 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(24.into(), 25.into())),
            }],
        };

        let _changed3 = d.apply_file_change(&change3);

        assert_eq!(d.content, "select id from users;\n-- ");
        assert_eq!(d.positions.len(), 1);

        let change4 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: "t".to_string(),
                range: Some(TextRange::new(25.into(), 26.into())),
            }],
        };

        let _changed4 = d.apply_file_change(&change4);

        assert_eq!(d.content, "select id from users;\n-- t");
        assert_eq!(d.positions.len(), 1);

        assert_document_integrity(&d);
    }

    #[test]
    fn within_statements() {
        let path = PgTPath::new("test.sql");
        let input = "select id from users;\n\n\n\nselect * from contacts;";

        let mut d = Document::new(input.to_string(), 0);

        assert_eq!(d.positions.len(), 2);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "select 1;".to_string(),
                range: Some(TextRange::new(23.into(), 23.into())),
            }],
        };

        let changed = d.apply_file_change(&change);

        assert_eq!(changed.len(), 5);
        assert_eq!(
            changed
                .iter()
                .filter(|c| matches!(c, StatementChange::Deleted(_)))
                .count(),
            2
        );
        assert_eq!(
            changed
                .iter()
                .filter(|c| matches!(c, StatementChange::Added(_)))
                .count(),
            3
        );

        assert_document_integrity(&d);
    }

    #[test]
    fn within_statements_2() {
        let path = PgTPath::new("test.sql");
        let input = "alter table deal alter column value drop not null;\n";
        let mut d = Document::new(input.to_string(), 0);

        assert_eq!(d.positions.len(), 1);

        let change1 = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(17.into(), 17.into())),
            }],
        };

        let changed1 = d.apply_file_change(&change1);
        assert_eq!(changed1.len(), 1);
        assert_eq!(
            d.content,
            "alter table deal  alter column value drop not null;\n"
        );
        assert_document_integrity(&d);

        let change2 = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(18.into(), 18.into())),
            }],
        };

        let changed2 = d.apply_file_change(&change2);
        assert_eq!(changed2.len(), 1);
        assert_eq!(
            d.content,
            "alter table deal   alter column value drop not null;\n"
        );
        assert_document_integrity(&d);

        let change3 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(19.into(), 19.into())),
            }],
        };

        let changed3 = d.apply_file_change(&change3);
        assert_eq!(changed3.len(), 1);
        assert_eq!(
            d.content,
            "alter table deal    alter column value drop not null;\n"
        );
        assert_document_integrity(&d);

        let change4 = ChangeFileParams {
            path: path.clone(),
            version: 4,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(20.into(), 20.into())),
            }],
        };

        let changed4 = d.apply_file_change(&change4);
        assert_eq!(changed4.len(), 1);
        assert_eq!(
            d.content,
            "alter table deal     alter column value drop not null;\n"
        );
        assert_document_integrity(&d);
    }

    #[test]
    fn julians_sample() {
        let path = PgTPath::new("test.sql");
        let input = "select\n  *\nfrom\n  test;\n\nselect\n\nalter table test\n\ndrop column id;";
        let mut d = Document::new(input.to_string(), 0);

        assert_eq!(d.positions.len(), 4);

        let change1 = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(31.into(), 31.into())),
            }],
        };

        let changed1 = d.apply_file_change(&change1);
        assert_eq!(changed1.len(), 1);
        assert_eq!(
            d.content,
            "select\n  *\nfrom\n  test;\n\nselect \n\nalter table test\n\ndrop column id;"
        );
        assert_document_integrity(&d);

        // problem: this creates a new statement
        let change2 = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: ";".to_string(),
                range: Some(TextRange::new(32.into(), 32.into())),
            }],
        };

        let changed2 = d.apply_file_change(&change2);
        assert_eq!(changed2.len(), 4);
        assert_eq!(
            changed2
                .iter()
                .filter(|c| matches!(c, StatementChange::Deleted(_)))
                .count(),
            2
        );
        assert_eq!(
            changed2
                .iter()
                .filter(|c| matches!(c, StatementChange::Added(_)))
                .count(),
            2
        );
        assert_document_integrity(&d);

        let change3 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(32.into(), 33.into())),
            }],
        };

        let changed3 = d.apply_file_change(&change3);
        assert_eq!(changed3.len(), 1);
        assert!(matches!(&changed3[0], StatementChange::Modified(_)));
        assert_eq!(
            d.content,
            "select\n  *\nfrom\n  test;\n\nselect \n\nalter table test\n\ndrop column id;"
        );
        match &changed3[0] {
            StatementChange::Modified(changed) => {
                assert_eq!(changed.old_stmt_text, "select ;");
                assert_eq!(changed.new_stmt_text, "select");
                assert_eq!(changed.change_text, "");
                assert_eq!(changed.change_range, TextRange::new(7.into(), 8.into()));
            }
            _ => panic!("expected modified statement"),
        }
        assert_document_integrity(&d);
    }

    #[test]
    fn across_statements() {
        let path = PgTPath::new("test.sql");
        let input = "select id from users;\nselect * from contacts;";

        let mut d = Document::new(input.to_string(), 0);

        assert_eq!(d.positions.len(), 2);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: ",test from users;\nselect 1;".to_string(),
                range: Some(TextRange::new(9.into(), 45.into())),
            }],
        };

        let changed = d.apply_file_change(&change);

        assert_eq!(changed.len(), 4);
        assert!(matches!(changed[0], StatementChange::Deleted(_)));
        assert_eq!(changed[0].statement().raw(), 1);
        assert!(matches!(
            changed[1],
            StatementChange::Deleted(StatementId::Root(_))
        ));
        assert_eq!(changed[1].statement().raw(), 0);
        assert!(
            matches!(&changed[2], StatementChange::Added(AddedStatement { stmt: _, text }) if text == "select id,test from users;")
        );
        assert!(
            matches!(&changed[3], StatementChange::Added(AddedStatement { stmt: _, text }) if text == "select 1;")
        );

        assert_document_integrity(&d);
    }

    #[test]
    fn append_whitespace_to_statement() {
        let path = PgTPath::new("test.sql");
        let input = "select id";

        let mut d = Document::new(input.to_string(), 0);

        assert_eq!(d.positions.len(), 1);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: " ".to_string(),
                range: Some(TextRange::new(9.into(), 10.into())),
            }],
        };

        let changed = d.apply_file_change(&change);

        assert_eq!(changed.len(), 1);

        assert_document_integrity(&d);
    }

    #[test]
    fn apply_changes() {
        let path = PgTPath::new("test.sql");
        let input = "select id from users;\nselect * from contacts;";

        let mut d = Document::new(input.to_string(), 0);

        assert_eq!(d.positions.len(), 2);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: ",test from users\nselect 1;".to_string(),
                range: Some(TextRange::new(9.into(), 45.into())),
            }],
        };

        let changed = d.apply_file_change(&change);

        assert_eq!(changed.len(), 4);

        assert!(matches!(
            changed[0],
            StatementChange::Deleted(StatementId::Root(_))
        ));
        assert_eq!(changed[0].statement().raw(), 1);
        assert!(matches!(
            changed[1],
            StatementChange::Deleted(StatementId::Root(_))
        ));
        assert_eq!(changed[1].statement().raw(), 0);
        assert_eq!(
            changed[2],
            StatementChange::Added(AddedStatement {
                stmt: StatementId::Root(2.into()),
                text: "select id,test from users".to_string()
            })
        );
        assert_eq!(
            changed[3],
            StatementChange::Added(AddedStatement {
                stmt: StatementId::Root(3.into()),
                text: "select 1;".to_string()
            })
        );

        assert_eq!("select id,test from users\nselect 1;", d.content);

        assert_document_integrity(&d);
    }

    #[test]
    fn removing_newline_at_the_beginning() {
        let path = PgTPath::new("test.sql");
        let input = "\n";

        let mut d = Document::new(input.to_string(), 1);

        assert_eq!(d.positions.len(), 0);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: "\nbegin;\n\nselect 1\n\nrollback;\n".to_string(),
                range: Some(TextRange::new(0.into(), 1.into())),
            }],
        };

        let changes = d.apply_file_change(&change);

        assert_eq!(changes.len(), 3);

        assert_document_integrity(&d);

        let change2 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(0.into(), 1.into())),
            }],
        };

        let changes2 = d.apply_file_change(&change2);

        assert_eq!(changes2.len(), 1);

        assert_document_integrity(&d);
    }

    #[test]
    fn apply_changes_at_end_of_statement() {
        let path = PgTPath::new("test.sql");
        let input = "select id from\nselect * from contacts;";

        let mut d = Document::new(input.to_string(), 1);

        assert_eq!(d.positions.len(), 2);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: " contacts;".to_string(),
                range: Some(TextRange::new(14.into(), 14.into())),
            }],
        };

        let changes = d.apply_file_change(&change);

        assert_eq!(changes.len(), 1);

        assert!(matches!(changes[0], StatementChange::Modified(_)));

        assert_eq!(
            "select id from contacts;\nselect * from contacts;",
            d.content
        );

        assert_document_integrity(&d);
    }

    #[test]
    fn apply_changes_replacement() {
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new("".to_string(), 0);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "select 1;\nselect 2;".to_string(),
                range: None,
            }],
        };

        doc.apply_file_change(&change);

        assert_eq!(doc.get_text(0), "select 1;".to_string());
        assert_eq!(doc.get_text(1), "select 2;".to_string());
        assert_eq!(
            doc.positions[0].1,
            TextRange::new(TextSize::new(0), TextSize::new(9))
        );
        assert_eq!(
            doc.positions[1].1,
            TextRange::new(TextSize::new(10), TextSize::new(19))
        );

        let change_2 = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(7.into(), 8.into())),
            }],
        };

        doc.apply_file_change(&change_2);

        assert_eq!(doc.content, "select ;\nselect 2;");
        assert_eq!(doc.positions.len(), 2);
        assert_eq!(doc.get_text(0), "select ;".to_string());
        assert_eq!(doc.get_text(1), "select 2;".to_string());
        assert_eq!(
            doc.positions[0].1,
            TextRange::new(TextSize::new(0), TextSize::new(8))
        );
        assert_eq!(
            doc.positions[1].1,
            TextRange::new(TextSize::new(9), TextSize::new(18))
        );

        let change_3 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: "!".to_string(),
                range: Some(TextRange::new(7.into(), 7.into())),
            }],
        };

        doc.apply_file_change(&change_3);

        assert_eq!(doc.content, "select !;\nselect 2;");
        assert_eq!(doc.positions.len(), 2);
        assert_eq!(
            doc.positions[0].1,
            TextRange::new(TextSize::new(0), TextSize::new(9))
        );
        assert_eq!(
            doc.positions[1].1,
            TextRange::new(TextSize::new(10), TextSize::new(19))
        );

        let change_4 = ChangeFileParams {
            path: path.clone(),
            version: 4,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(7.into(), 8.into())),
            }],
        };

        doc.apply_file_change(&change_4);

        assert_eq!(doc.content, "select ;\nselect 2;");
        assert_eq!(doc.positions.len(), 2);
        assert_eq!(
            doc.positions[0].1,
            TextRange::new(TextSize::new(0), TextSize::new(8))
        );
        assert_eq!(
            doc.positions[1].1,
            TextRange::new(TextSize::new(9), TextSize::new(18))
        );

        let change_5 = ChangeFileParams {
            path: path.clone(),
            version: 5,
            changes: vec![ChangeParams {
                text: "1".to_string(),
                range: Some(TextRange::new(7.into(), 7.into())),
            }],
        };

        doc.apply_file_change(&change_5);

        assert_eq!(doc.content, "select 1;\nselect 2;");
        assert_eq!(doc.positions.len(), 2);
        assert_eq!(
            doc.positions[0].1,
            TextRange::new(TextSize::new(0), TextSize::new(9))
        );
        assert_eq!(
            doc.positions[1].1,
            TextRange::new(TextSize::new(10), TextSize::new(19))
        );

        assert_document_integrity(&doc);
    }

    #[test]
    fn comment_at_begin() {
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new(
            "-- Add new schema named \"private\"\nCREATE SCHEMA \"private\";".to_string(),
            0,
        );

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(0.into(), 1.into())),
            }],
        };

        let changed = doc.apply_file_change(&change);

        assert_eq!(
            doc.content,
            "- Add new schema named \"private\"\nCREATE SCHEMA \"private\";"
        );
        assert_eq!(changed.len(), 3);
        assert!(matches!(&changed[0], StatementChange::Deleted(_)));
        assert!(matches!(
            changed[1],
            StatementChange::Added(AddedStatement { .. })
        ));
        assert!(matches!(
            changed[2],
            StatementChange::Added(AddedStatement { .. })
        ));

        let change_2 = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: "-".to_string(),
                range: Some(TextRange::new(0.into(), 0.into())),
            }],
        };

        let changed_2 = doc.apply_file_change(&change_2);

        assert_eq!(
            doc.content,
            "-- Add new schema named \"private\"\nCREATE SCHEMA \"private\";"
        );

        assert_eq!(changed_2.len(), 3);
        assert!(matches!(
            changed_2[0],
            StatementChange::Deleted(StatementId::Root(_))
        ));
        assert!(matches!(
            changed_2[1],
            StatementChange::Deleted(StatementId::Root(_))
        ));
        assert!(matches!(
            changed_2[2],
            StatementChange::Added(AddedStatement { .. })
        ));

        assert_document_integrity(&doc);
    }

    #[test]
    fn apply_changes_within_statement() {
        let input = "select id  from users;\nselect * from contacts;";
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new(input.to_string(), 0);

        assert_eq!(doc.positions.len(), 2);

        let stmt_1_range = doc.positions[0].clone();
        let stmt_2_range = doc.positions[1].clone();

        let update_text = ",test";

        let update_range = TextRange::new(9.into(), 10.into());

        let update_text_len = u32::try_from(update_text.chars().count()).unwrap();
        let update_addition = update_text_len - u32::from(update_range.len());

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: update_text.to_string(),
                range: Some(update_range),
            }],
        };

        doc.apply_file_change(&change);

        assert_eq!(
            "select id,test from users;\nselect * from contacts;",
            doc.content
        );
        assert_eq!(doc.positions.len(), 2);
        assert_eq!(doc.positions[0].1.start(), stmt_1_range.1.start());
        assert_eq!(
            u32::from(doc.positions[0].1.end()),
            u32::from(stmt_1_range.1.end()) + update_addition
        );
        assert_eq!(
            u32::from(doc.positions[1].1.start()),
            u32::from(stmt_2_range.1.start()) + update_addition
        );
        assert_eq!(
            u32::from(doc.positions[1].1.end()),
            u32::from(stmt_2_range.1.end()) + update_addition
        );

        assert_document_integrity(&doc);
    }

    #[test]
    fn remove_outside_of_content() {
        let path = PgTPath::new("test.sql");
        let input = "select id from contacts;\n\nselect * from contacts;";

        let mut d = Document::new(input.to_string(), 1);

        assert_eq!(d.positions.len(), 2);

        let change1 = ChangeFileParams {
            path: path.clone(),
            version: 2,
            changes: vec![ChangeParams {
                text: "\n".to_string(),
                range: Some(TextRange::new(49.into(), 49.into())),
            }],
        };

        d.apply_file_change(&change1);

        assert_eq!(
            d.content,
            "select id from contacts;\n\nselect * from contacts;\n"
        );

        let change2 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: "\n".to_string(),
                range: Some(TextRange::new(50.into(), 50.into())),
            }],
        };

        d.apply_file_change(&change2);

        assert_eq!(
            d.content,
            "select id from contacts;\n\nselect * from contacts;\n\n"
        );

        let change5 = ChangeFileParams {
            path: path.clone(),
            version: 6,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(51.into(), 52.into())),
            }],
        };

        let changes = d.apply_file_change(&change5);

        assert!(matches!(
            changes[0],
            StatementChange::Deleted(StatementId::Root(_))
        ));

        assert!(matches!(
            changes[1],
            StatementChange::Added(AddedStatement { .. })
        ));

        assert_eq!(changes.len(), 2);

        assert_eq!(
            d.content,
            "select id from contacts;\n\nselect * from contacts;\n\n"
        );

        assert_document_integrity(&d);
    }

    #[test]
    fn remove_trailing_whitespace() {
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new("select * from ".to_string(), 0);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(13.into(), 14.into())),
            }],
        };

        let changed = doc.apply_file_change(&change);

        assert_eq!(doc.content, "select * from");

        assert_eq!(changed.len(), 1);

        match &changed[0] {
            StatementChange::Modified(stmt) => {
                let ModifiedStatement {
                    change_range,
                    change_text,
                    new_stmt_text,
                    old_stmt_text,
                    ..
                } = stmt;

                assert_eq!(change_range, &TextRange::new(13.into(), 14.into()));
                assert_eq!(change_text, "");
                assert_eq!(new_stmt_text, "select * from");

                // the whitespace was not considered
                // to be a part of the statement
                assert_eq!(old_stmt_text, "select * from");
            }

            _ => unreachable!("Did not yield a modified statement."),
        }

        assert_document_integrity(&doc);
    }

    #[test]
    fn remove_trailing_whitespace_and_last_char() {
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new("select * from ".to_string(), 0);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(12.into(), 14.into())),
            }],
        };

        let changed = doc.apply_file_change(&change);

        assert_eq!(doc.content, "select * fro");

        assert_eq!(changed.len(), 1);

        match &changed[0] {
            StatementChange::Modified(stmt) => {
                let ModifiedStatement {
                    change_range,
                    change_text,
                    new_stmt_text,
                    old_stmt_text,
                    ..
                } = stmt;

                assert_eq!(change_range, &TextRange::new(12.into(), 14.into()));
                assert_eq!(change_text, "");
                assert_eq!(new_stmt_text, "select * fro");

                // the whitespace was not considered
                // to be a part of the statement
                assert_eq!(old_stmt_text, "select * from");
            }

            _ => unreachable!("Did not yield a modified statement."),
        }

        assert_document_integrity(&doc);
    }

    #[test]
    fn multiple_deletions_at_once() {
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new("ALTER TABLE ONLY public.omni_channel_message ADD CONSTRAINT omni_channel_message_organisation_id_fkey FOREIGN KEY (organisation_id) REFERENCES public.organisation(id) ON UPDATE RESTRICT ON DELETE CASCADE;".to_string(), 0);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![
                ChangeParams {
                    range: Some(TextRange::new(60.into(), 80.into())),
                    text: "sendout".to_string(),
                },
                ChangeParams {
                    range: Some(TextRange::new(24.into(), 44.into())),
                    text: "sendout".to_string(),
                },
            ],
        };

        let changed = doc.apply_file_change(&change);

        assert_eq!(
            doc.content,
            "ALTER TABLE ONLY public.sendout ADD CONSTRAINT sendout_organisation_id_fkey FOREIGN KEY (organisation_id) REFERENCES public.organisation(id) ON UPDATE RESTRICT ON DELETE CASCADE;"
        );

        assert_eq!(changed.len(), 2);

        assert_document_integrity(&doc);
    }

    #[test]
    fn multiple_additions_at_once() {
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new("ALTER TABLE ONLY public.sendout ADD CONSTRAINT sendout_organisation_id_fkey FOREIGN KEY (organisation_id) REFERENCES public.organisation(id) ON UPDATE RESTRICT ON DELETE CASCADE;".to_string(), 0);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![
                ChangeParams {
                    range: Some(TextRange::new(47.into(), 54.into())),
                    text: "omni_channel_message".to_string(),
                },
                ChangeParams {
                    range: Some(TextRange::new(24.into(), 31.into())),
                    text: "omni_channel_message".to_string(),
                },
            ],
        };

        let changed = doc.apply_file_change(&change);

        assert_eq!(
            doc.content,
            "ALTER TABLE ONLY public.omni_channel_message ADD CONSTRAINT omni_channel_message_organisation_id_fkey FOREIGN KEY (organisation_id) REFERENCES public.organisation(id) ON UPDATE RESTRICT ON DELETE CASCADE;"
        );

        assert_eq!(changed.len(), 2);

        assert_document_integrity(&doc);
    }

    #[test]
    fn remove_inbetween_whitespace() {
        let path = PgTPath::new("test.sql");

        let mut doc = Document::new("select *   from users".to_string(), 0);

        let change = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(9.into(), 11.into())),
            }],
        };

        let changed = doc.apply_file_change(&change);

        assert_eq!(doc.content, "select * from users");

        assert_eq!(changed.len(), 1);

        match &changed[0] {
            StatementChange::Modified(stmt) => {
                let ModifiedStatement {
                    change_range,
                    change_text,
                    new_stmt_text,
                    old_stmt_text,
                    ..
                } = stmt;

                assert_eq!(change_range, &TextRange::new(9.into(), 11.into()));
                assert_eq!(change_text, "");
                assert_eq!(old_stmt_text, "select *   from users");
                assert_eq!(new_stmt_text, "select * from users");
            }

            _ => unreachable!("Did not yield a modified statement."),
        }

        assert_document_integrity(&doc);
    }

    #[test]
    fn test_another_issue() {
        let path = PgTPath::new("test.sql");
        let initial_content = r#"



ALTER TABLE ONLY "public"."campaign_contact_list"
    ADD CONSTRAINT "campaign_contact_list_contact_list_id_fkey" FOREIGN KEY ("contact_list_id") REFERENCES "public"."contact_list"("id") ON UPDATE RESTRICT ON DELETE CASCADE;
"#;

        let mut doc = Document::new(initial_content.to_string(), 0);

        let change1 = ChangeFileParams {
            path: path.clone(),
            version: 1,
            changes: vec![
                ChangeParams {
                    range: Some(TextRange::new(31.into(), 39.into())),
                    text: "journey_node".to_string(),
                },
                ChangeParams {
                    range: Some(TextRange::new(74.into(), 82.into())),
                    text: "journey_node".to_string(),
                },
            ],
        };

        let _changes = doc.apply_file_change(&change1);

        let expected_content = r#"



ALTER TABLE ONLY "public"."journey_node_contact_list"
    ADD CONSTRAINT "journey_node_contact_list_contact_list_id_fkey" FOREIGN KEY ("contact_list_id") REFERENCES "public"."contact_list"("id") ON UPDATE RESTRICT ON DELETE CASCADE;
"#;

        assert_eq!(doc.content, expected_content);

        assert_document_integrity(&doc);
    }

    #[test]
    fn test_comments_only() {
        let path = PgTPath::new("test.sql");
        let initial_content = "-- atlas:import async_trigger/setup.sql\n-- atlas:import public/setup.sql\n-- atlas:import private/setup.sql\n-- atlas:import api/setup.sql\n-- atlas:import async_trigger/index.sql\n-- atlas:import public/enums/index.sql\n-- atlas:import public/types/index.sql\n-- atlas:import private/enums/index.sql\n-- atlas:import private/functions/index.sql\n-- atlas:import public/tables/index.sql\n-- atlas:import public/index.sql\n-- atlas:import private/index.sql\n-- atlas:import api/index.sql\n\n\n\n";

        // Create a new document
        let mut doc = Document::new(initial_content.to_string(), 0);

        // First change: Delete some text at line 2, character 24-29
        let change1 = ChangeFileParams {
            path: path.clone(),
            version: 3,
            changes: vec![ChangeParams {
                text: "".to_string(),
                range: Some(TextRange::new(
                    // Calculate the correct position based on the content
                    // Line 2, character 24
                    98.into(),
                    // Line 2, character 29
                    103.into(),
                )),
            }],
        };

        let _changes1 = doc.apply_file_change(&change1);

        // Second change: Add 't' at line 2, character 24
        let change2 = ChangeFileParams {
            path: path.clone(),
            version: 4,
            changes: vec![ChangeParams {
                text: "t".to_string(),
                range: Some(TextRange::new(98.into(), 98.into())),
            }],
        };

        let _changes2 = doc.apply_file_change(&change2);

        assert_eq!(
            doc.positions.len(),
            0,
            "Document should have no statement after adding 't'"
        );

        // Third change: Add 'e' at line 2, character 25
        let change3 = ChangeFileParams {
            path: path.clone(),
            version: 5,
            changes: vec![ChangeParams {
                text: "e".to_string(),
                range: Some(TextRange::new(99.into(), 99.into())),
            }],
        };

        let _changes3 = doc.apply_file_change(&change3);
        assert_eq!(
            doc.positions.len(),
            0,
            "Document should still have no statement"
        );

        // Fourth change: Add 's' at line 2, character 26
        let change4 = ChangeFileParams {
            path: path.clone(),
            version: 6,
            changes: vec![ChangeParams {
                text: "s".to_string(),
                range: Some(TextRange::new(100.into(), 100.into())),
            }],
        };

        let _changes4 = doc.apply_file_change(&change4);
        assert_eq!(
            doc.positions.len(),
            0,
            "Document should still have no statement"
        );

        // Fifth change: Add 't' at line 2, character 27
        let change5 = ChangeFileParams {
            path: path.clone(),
            version: 7,
            changes: vec![ChangeParams {
                text: "t".to_string(),
                range: Some(TextRange::new(101.into(), 101.into())),
            }],
        };

        let _changes5 = doc.apply_file_change(&change5);
        assert_eq!(
            doc.positions.len(),
            0,
            "Document should still have no statement"
        );

        assert_document_integrity(&doc);
    }
}
