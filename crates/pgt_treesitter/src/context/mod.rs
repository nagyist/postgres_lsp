use std::{
    cmp,
    collections::{HashMap, HashSet},
};
mod base_parser;
mod grant_parser;
mod policy_parser;
mod revoke_parser;

use crate::queries::{self, QueryResult, TreeSitterQueriesExecutor};
use pgt_text_size::{TextRange, TextSize};

use crate::context::{
    base_parser::CompletionStatementParser,
    grant_parser::GrantParser,
    policy_parser::{PolicyParser, PolicyStmtKind},
    revoke_parser::RevokeParser,
};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum WrappingClause<'a> {
    Select,
    Where,
    From,
    Join {
        on_node: Option<tree_sitter::Node<'a>>,
    },
    Update,
    Delete,
    ColumnDefinitions,
    Insert,
    AlterTable,
    DropTable,
    DropColumn,
    AlterColumn,
    RenameColumn,
    PolicyName,
    ToRoleAssignment,
    SetStatement,
    AlterRole,
    DropRole,

    /// `PolicyCheck` refers to either the `WITH CHECK` or the `USING` clause
    /// in a policy statement.
    /// ```sql
    /// CREATE POLICY "my pol" ON PUBLIC.USERS
    /// FOR SELECT
    /// USING (...) -- this one!
    /// ```
    PolicyCheck,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub struct MentionedColumn {
    pub column: String,
    pub alias: Option<String>,
}

/// We can map a few nodes, such as the "update" node, to actual SQL clauses.
/// That gives us a lot of insight for completions.
/// Other nodes, such as the "relation" node, gives us less but still
/// relevant information.
/// `WrappingNode` maps to such nodes.
///
/// Note: This is not the direct parent of the `node_under_cursor`, but the closest
/// *relevant* parent.
#[derive(Debug, PartialEq, Eq)]
pub enum WrappingNode {
    Relation,
    BinaryExpression,
    Assignment,
    List,
}

#[derive(Debug)]
pub enum NodeUnderCursor<'a> {
    TsNode(tree_sitter::Node<'a>),
    CustomNode {
        text: String,
        range: TextRange,
        kind: String,
        previous_node_kind: Option<String>,
    },
}

impl NodeUnderCursor<'_> {
    pub fn start_byte(&self) -> usize {
        match self {
            NodeUnderCursor::TsNode(node) => node.start_byte(),
            NodeUnderCursor::CustomNode { range, .. } => range.start().into(),
        }
    }

    pub fn end_byte(&self) -> usize {
        match self {
            NodeUnderCursor::TsNode(node) => node.end_byte(),
            NodeUnderCursor::CustomNode { range, .. } => range.end().into(),
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            NodeUnderCursor::TsNode(node) => node.kind(),
            NodeUnderCursor::CustomNode { kind, .. } => kind.as_str(),
        }
    }
}

impl<'a> From<tree_sitter::Node<'a>> for NodeUnderCursor<'a> {
    fn from(node: tree_sitter::Node<'a>) -> Self {
        NodeUnderCursor::TsNode(node)
    }
}

impl TryFrom<&str> for WrappingNode {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "relation" => Ok(Self::Relation),
            "assignment" => Ok(Self::Assignment),
            "binary_expression" => Ok(Self::BinaryExpression),
            "list" => Ok(Self::List),
            _ => {
                let message = format!("Unimplemented Relation: {}", value);

                // Err on tests, so we notice that we're lacking an implementation immediately.
                if cfg!(test) {
                    panic!("{}", message);
                }

                Err(message)
            }
        }
    }
}

impl TryFrom<String> for WrappingNode {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

pub struct TreeSitterContextParams<'a> {
    pub position: TextSize,
    pub text: &'a str,
    pub tree: &'a tree_sitter::Tree,
}

#[derive(Debug)]
pub struct TreesitterContext<'a> {
    pub node_under_cursor: Option<NodeUnderCursor<'a>>,

    pub tree: &'a tree_sitter::Tree,
    pub text: &'a str,
    pub position: usize,

    /// If the cursor is on a node that uses dot notation
    /// to specify an alias or schema, this will hold the schema's or
    /// alias's name.
    ///
    /// Here, `auth` is a schema name:
    /// ```sql
    /// select * from auth.users;
    /// ```
    ///
    /// Here, `u` is an alias name:
    /// ```sql
    /// select
    ///     *
    /// from
    ///     auth.users u
    ///     left join identities i
    ///     on u.id = i.user_id;
    /// ```
    pub schema_or_alias_name: Option<String>,

    pub wrapping_clause_type: Option<WrappingClause<'a>>,

    pub wrapping_node_kind: Option<WrappingNode>,

    pub is_invocation: bool,
    pub wrapping_statement_range: Option<tree_sitter::Range>,

    pub mentioned_relations: HashMap<Option<String>, HashSet<String>>,
    pub mentioned_table_aliases: HashMap<String, String>,
    pub mentioned_columns: HashMap<Option<WrappingClause<'a>>, HashSet<MentionedColumn>>,
}

impl<'a> TreesitterContext<'a> {
    pub fn new(params: TreeSitterContextParams<'a>) -> Self {
        let mut ctx = Self {
            tree: params.tree,
            text: params.text,
            position: usize::from(params.position),
            node_under_cursor: None,
            schema_or_alias_name: None,
            wrapping_clause_type: None,
            wrapping_node_kind: None,
            wrapping_statement_range: None,
            is_invocation: false,
            mentioned_relations: HashMap::new(),
            mentioned_table_aliases: HashMap::new(),
            mentioned_columns: HashMap::new(),
        };

        // policy handling is important to Supabase, but they are a PostgreSQL specific extension,
        // so the tree_sitter_sql language does not support it.
        // We infer the context manually.
        if PolicyParser::looks_like_matching_stmt(params.text) {
            ctx.gather_policy_context();
        } else if GrantParser::looks_like_matching_stmt(params.text) {
            ctx.gather_grant_context();
        } else if RevokeParser::looks_like_matching_stmt(params.text) {
            ctx.gather_revoke_context();
        } else {
            ctx.gather_tree_context();
            ctx.gather_info_from_ts_queries();
        }

        ctx
    }

    fn gather_revoke_context(&mut self) {
        let revoke_context = RevokeParser::get_context(self.text, self.position);

        self.node_under_cursor = Some(NodeUnderCursor::CustomNode {
            text: revoke_context.node_text,
            range: revoke_context.node_range,
            kind: revoke_context.node_kind.clone(),
            previous_node_kind: None,
        });

        if revoke_context.node_kind == "revoke_table" {
            self.schema_or_alias_name = revoke_context.schema_name.clone();
        }

        if revoke_context.table_name.is_some() {
            let mut new = HashSet::new();
            new.insert(revoke_context.table_name.unwrap());
            self.mentioned_relations
                .insert(revoke_context.schema_name, new);
        }

        self.wrapping_clause_type = match revoke_context.node_kind.as_str() {
            "revoke_role" => Some(WrappingClause::ToRoleAssignment),
            "revoke_table" => Some(WrappingClause::From),
            _ => None,
        };
    }

    fn gather_grant_context(&mut self) {
        let grant_context = GrantParser::get_context(self.text, self.position);

        self.node_under_cursor = Some(NodeUnderCursor::CustomNode {
            text: grant_context.node_text,
            range: grant_context.node_range,
            kind: grant_context.node_kind.clone(),
            previous_node_kind: None,
        });

        if grant_context.node_kind == "grant_table" {
            self.schema_or_alias_name = grant_context.schema_name.clone();
        }

        if grant_context.table_name.is_some() {
            let mut new = HashSet::new();
            new.insert(grant_context.table_name.unwrap());
            self.mentioned_relations
                .insert(grant_context.schema_name, new);
        }

        self.wrapping_clause_type = match grant_context.node_kind.as_str() {
            "grant_role" => Some(WrappingClause::ToRoleAssignment),
            "grant_table" => Some(WrappingClause::From),
            _ => None,
        };
    }

    fn gather_policy_context(&mut self) {
        let policy_context = PolicyParser::get_context(self.text, self.position);

        self.node_under_cursor = Some(NodeUnderCursor::CustomNode {
            text: policy_context.node_text,
            range: policy_context.node_range,
            kind: policy_context.node_kind.clone(),
            previous_node_kind: Some(policy_context.previous_node_kind),
        });

        if policy_context.node_kind == "policy_table" {
            self.schema_or_alias_name = policy_context.schema_name.clone();
        }

        if policy_context.table_name.is_some() {
            let mut new = HashSet::new();
            new.insert(policy_context.table_name.unwrap());
            self.mentioned_relations
                .insert(policy_context.schema_name, new);
        }

        self.wrapping_clause_type = match policy_context.node_kind.as_str() {
            "policy_name" if policy_context.statement_kind != PolicyStmtKind::Create => {
                Some(WrappingClause::PolicyName)
            }
            "policy_role" => Some(WrappingClause::ToRoleAssignment),
            "policy_table" => Some(WrappingClause::From),
            _ => {
                if policy_context.in_check_or_using_clause {
                    Some(WrappingClause::PolicyCheck)
                } else {
                    None
                }
            }
        };
    }

    fn gather_info_from_ts_queries(&mut self) {
        let stmt_range = self.wrapping_statement_range.as_ref();
        let sql = self.text;

        let mut executor = TreeSitterQueriesExecutor::new(self.tree.root_node(), sql);

        executor.add_query_results::<queries::RelationMatch>();
        executor.add_query_results::<queries::TableAliasMatch>();
        executor.add_query_results::<queries::SelectColumnMatch>();
        executor.add_query_results::<queries::InsertColumnMatch>();
        executor.add_query_results::<queries::WhereColumnMatch>();

        for relation_match in executor.get_iter(stmt_range) {
            match relation_match {
                QueryResult::Relation(r) => {
                    let schema_name = r.get_schema(sql);
                    let table_name = r.get_table(sql);

                    self.mentioned_relations
                        .entry(schema_name)
                        .and_modify(|s| {
                            s.insert(table_name.clone());
                        })
                        .or_insert(HashSet::from([table_name]));
                }

                QueryResult::TableAliases(table_alias_match) => {
                    self.mentioned_table_aliases.insert(
                        table_alias_match.get_alias(sql),
                        table_alias_match.get_table(sql),
                    );
                }

                QueryResult::SelectClauseColumns(c) => {
                    let mentioned = MentionedColumn {
                        column: c.get_column(sql),
                        alias: c.get_alias(sql),
                    };

                    self.mentioned_columns
                        .entry(Some(WrappingClause::Select))
                        .and_modify(|s| {
                            s.insert(mentioned.clone());
                        })
                        .or_insert(HashSet::from([mentioned]));
                }

                QueryResult::WhereClauseColumns(c) => {
                    let mentioned = MentionedColumn {
                        column: c.get_column(sql),
                        alias: c.get_alias(sql),
                    };

                    self.mentioned_columns
                        .entry(Some(WrappingClause::Where))
                        .and_modify(|s| {
                            s.insert(mentioned.clone());
                        })
                        .or_insert(HashSet::from([mentioned]));
                }

                QueryResult::InsertClauseColumns(c) => {
                    let mentioned = MentionedColumn {
                        column: c.get_column(sql),
                        alias: None,
                    };

                    self.mentioned_columns
                        .entry(Some(WrappingClause::Insert))
                        .and_modify(|s| {
                            s.insert(mentioned.clone());
                        })
                        .or_insert(HashSet::from([mentioned]));
                }
                _ => {}
            };
        }
    }

    fn get_ts_node_content(&self, ts_node: &tree_sitter::Node<'a>) -> Option<String> {
        let source = self.text;
        ts_node
            .utf8_text(source.as_bytes())
            .ok()
            .map(|txt| txt.into())
    }

    pub fn get_node_under_cursor_content(&self) -> Option<String> {
        match self.node_under_cursor.as_ref()? {
            NodeUnderCursor::TsNode(node) => self.get_ts_node_content(node),
            NodeUnderCursor::CustomNode { text, .. } => Some(text.clone()),
        }
    }

    fn gather_tree_context(&mut self) {
        let mut cursor = self.tree.root_node().walk();

        /*
         * The head node of any treesitter tree is always the "PROGRAM" node.
         *
         * We want to enter the next layer and focus on the child node that matches the user's cursor position.
         * If there is no node under the users position, however, the cursor won't enter the next level – it
         * will stay on the Program node.
         *
         * This might lead to an unexpected context or infinite recursion.
         *
         * We'll therefore adjust the cursor position such that it meets the last node of the AST.
         * `select * from use           {}` becomes `select * from use{}`.
         */
        let current_node = cursor.node();

        let mut chars = self.text.chars();

        if chars
            .nth(self.position)
            .is_some_and(|c| !c.is_ascii_whitespace() && !&[';', ')'].contains(&c))
        {
            self.position = cmp::min(self.position + 1, self.text.len());
        } else {
            self.position = cmp::min(self.position, self.text.len());
        }

        cursor.goto_first_child_for_byte(self.position);

        self.gather_context_from_node(cursor, current_node);
    }

    fn gather_context_from_node(
        &mut self,
        mut cursor: tree_sitter::TreeCursor<'a>,
        parent_node: tree_sitter::Node<'a>,
    ) {
        let current_node = cursor.node();

        let parent_node_kind = parent_node.kind();
        let current_node_kind = current_node.kind();

        // prevent infinite recursion – this can happen with ERROR nodes
        if current_node_kind == parent_node_kind && ["ERROR", "program"].contains(&parent_node_kind)
        {
            self.node_under_cursor = Some(NodeUnderCursor::from(current_node));
            return;
        }

        match parent_node_kind {
            "statement" | "subquery" => {
                self.wrapping_clause_type =
                    self.get_wrapping_clause_from_current_node(current_node, &mut cursor);

                self.wrapping_statement_range = Some(parent_node.range());
            }
            "invocation" => self.is_invocation = true,
            _ => {}
        }

        // try to gather context from the siblings if we're within an error node.
        if parent_node_kind == "ERROR" {
            if let Some(clause_type) = self.get_wrapping_clause_from_error_node_child(current_node)
            {
                self.wrapping_clause_type = Some(clause_type);
            }
            if let Some(wrapping_node) = self.get_wrapping_node_from_error_node_child(current_node)
            {
                self.wrapping_node_kind = Some(wrapping_node)
            }

            self.get_info_from_error_node_child(current_node);
        }

        match current_node_kind {
            "object_reference" | "field" => {
                let content = self.get_ts_node_content(&current_node);
                if let Some(txt) = content {
                    let parts: Vec<&str> = txt.split('.').collect();
                    if parts.len() == 2 {
                        self.schema_or_alias_name = Some(parts[0].to_string());
                    }
                }
            }

            "where" | "update" | "select" | "delete" | "from" | "join" | "column_definitions"
            | "alter_role" | "drop_role" | "set_statement" | "drop_table" | "alter_table"
            | "drop_column" | "alter_column" | "rename_column" => {
                self.wrapping_clause_type =
                    self.get_wrapping_clause_from_current_node(current_node, &mut cursor);
            }

            "relation" | "binary_expression" | "assignment" => {
                self.wrapping_node_kind = current_node_kind.try_into().ok();
            }

            "list" => {
                if current_node
                    .prev_sibling()
                    .is_none_or(|n| n.kind() != "keyword_values")
                {
                    self.wrapping_node_kind = current_node_kind.try_into().ok();
                }
            }

            _ => {}
        }

        // We have arrived at the leaf node
        if current_node.child_count() == 0 {
            self.node_under_cursor = Some(NodeUnderCursor::from(current_node));
            return;
        }

        cursor.goto_first_child_for_byte(self.position);
        self.gather_context_from_node(cursor, current_node);
    }

    fn get_first_sibling(&self, node: tree_sitter::Node<'a>) -> tree_sitter::Node<'a> {
        let mut first_sibling = node;
        while let Some(n) = first_sibling.prev_sibling() {
            first_sibling = n;
        }
        first_sibling
    }

    fn get_wrapping_node_from_error_node_child(
        &self,
        node: tree_sitter::Node<'a>,
    ) -> Option<WrappingNode> {
        self.wrapping_clause_type
            .as_ref()
            .and_then(|clause| match clause {
                WrappingClause::Insert => {
                    let mut first_sib = self.get_first_sibling(node);

                    let mut after_opening_bracket = false;
                    let mut before_closing_bracket = false;

                    while let Some(next_sib) = first_sib.next_sibling() {
                        if next_sib.kind() == "("
                            && next_sib.end_position() <= node.start_position()
                        {
                            after_opening_bracket = true;
                        }

                        if next_sib.kind() == ")"
                            && next_sib.start_position() >= node.end_position()
                        {
                            before_closing_bracket = true;
                        }

                        first_sib = next_sib;
                    }

                    if after_opening_bracket && before_closing_bracket {
                        Some(WrappingNode::List)
                    } else {
                        None
                    }
                }
                _ => None,
            })
    }

    fn get_wrapping_clause_from_error_node_child(
        &self,
        node: tree_sitter::Node<'a>,
    ) -> Option<WrappingClause<'a>> {
        let clause_combinations: Vec<(WrappingClause, &[&'static str])> = vec![
            (WrappingClause::Where, &["where"]),
            (WrappingClause::Update, &["update"]),
            (WrappingClause::Select, &["select"]),
            (WrappingClause::Delete, &["delete"]),
            (WrappingClause::Insert, &["insert", "into"]),
            (WrappingClause::From, &["from"]),
            (WrappingClause::Join { on_node: None }, &["join"]),
            (WrappingClause::AlterTable, &["alter", "table"]),
            (WrappingClause::AlterColumn, &["alter", "table", "alter"]),
            (WrappingClause::RenameColumn, &["alter", "table", "rename"]),
            (
                WrappingClause::AlterTable,
                &["alter", "table", "if", "exists"],
            ),
            (WrappingClause::DropTable, &["drop", "table"]),
            (
                WrappingClause::DropTable,
                &["drop", "table", "if", "exists"],
            ),
        ];

        let first_sibling = self.get_first_sibling(node);

        /*
         * For each clause, we'll iterate from first_sibling to the next ones,
         * either until the end or until we land on the node under the cursor.
         * We'll score the `WrappingClause` by how many tokens it matches in order.
         */
        let mut clauses_with_score: Vec<(WrappingClause, usize)> = clause_combinations
            .into_iter()
            .map(|(clause, tokens)| {
                let mut idx = 0;

                let mut sibling = Some(first_sibling);
                while let Some(sib) = sibling {
                    if sib.end_byte() >= node.end_byte() || idx >= tokens.len() {
                        break;
                    }

                    if let Some(sibling_content) = self.get_ts_node_content(&sib) {
                        if sibling_content == tokens[idx] {
                            idx += 1;
                        }
                    } else {
                        break;
                    }

                    sibling = sib.next_sibling();
                }

                (clause, idx)
            })
            .collect();

        clauses_with_score.sort_by(|(_, score_a), (_, score_b)| score_b.cmp(score_a));
        clauses_with_score
            .iter()
            .find(|(_, score)| *score > 0)
            .map(|c| c.0.clone())
    }

    fn get_info_from_error_node_child(&mut self, node: tree_sitter::Node<'a>) {
        let mut first_sibling = self.get_first_sibling(node);

        if let Some(clause) = self.wrapping_clause_type.as_ref() {
            match *clause {
                WrappingClause::Insert => {
                    while let Some(sib) = first_sibling.next_sibling() {
                        match sib.kind() {
                            "object_reference" => {
                                if let Some(txt) = self.get_ts_node_content(&sib) {
                                    let mut iter = txt.split('.').rev();
                                    let table = iter.next().unwrap().to_string();
                                    let schema = iter.next().map(|s| s.to_string());
                                    self.mentioned_relations
                                        .entry(schema)
                                        .and_modify(|s| {
                                            s.insert(table.clone());
                                        })
                                        .or_insert(HashSet::from([table]));
                                }
                            }

                            "column" => {
                                if let Some(txt) = self.get_ts_node_content(&sib) {
                                    let entry = MentionedColumn {
                                        column: txt,
                                        alias: None,
                                    };

                                    self.mentioned_columns
                                        .entry(Some(WrappingClause::Insert))
                                        .and_modify(|s| {
                                            s.insert(entry.clone());
                                        })
                                        .or_insert(HashSet::from([entry]));
                                }
                            }

                            _ => {}
                        }

                        first_sibling = sib;
                    }
                }

                WrappingClause::AlterColumn => {
                    while let Some(sib) = first_sibling.next_sibling() {
                        if sib.kind() == "object_reference" {
                            if let Some(txt) = self.get_ts_node_content(&sib) {
                                let mut iter = txt.split('.').rev();
                                let table = iter.next().unwrap().to_string();
                                let schema = iter.next().map(|s| s.to_string());
                                self.mentioned_relations
                                    .entry(schema)
                                    .and_modify(|s| {
                                        s.insert(table.clone());
                                    })
                                    .or_insert(HashSet::from([table]));
                            }
                        }

                        first_sibling = sib;
                    }
                }

                _ => {}
            }
        }
    }

    fn get_wrapping_clause_from_current_node(
        &self,
        node: tree_sitter::Node<'a>,
        cursor: &mut tree_sitter::TreeCursor<'a>,
    ) -> Option<WrappingClause<'a>> {
        match node.kind() {
            "where" => Some(WrappingClause::Where),
            "update" => Some(WrappingClause::Update),
            "select" => Some(WrappingClause::Select),
            "delete" => Some(WrappingClause::Delete),
            "from" => Some(WrappingClause::From),
            "drop_table" => Some(WrappingClause::DropTable),
            "alter_role" => Some(WrappingClause::AlterRole),
            "drop_role" => Some(WrappingClause::DropRole),
            "drop_column" => Some(WrappingClause::DropColumn),
            "alter_column" => Some(WrappingClause::AlterColumn),
            "rename_column" => Some(WrappingClause::RenameColumn),
            "alter_table" => Some(WrappingClause::AlterTable),
            "set_statement" => Some(WrappingClause::SetStatement),
            "column_definitions" => Some(WrappingClause::ColumnDefinitions),
            "insert" => Some(WrappingClause::Insert),
            "join" => {
                // sadly, we need to manually iterate over the children –
                // `node.child_by_field_id(..)` does not work as expected
                let mut on_node = None;
                for child in node.children(cursor) {
                    // 28 is the id for "keyword_on"
                    if child.kind_id() == 28 {
                        on_node = Some(child);
                    }
                }
                cursor.goto_parent();
                Some(WrappingClause::Join { on_node })
            }
            _ => None,
        }
    }

    pub fn parent_matches_one_of_kind(&self, kinds: &[&'static str]) -> bool {
        self.node_under_cursor
            .as_ref()
            .is_some_and(|under_cursor| match under_cursor {
                NodeUnderCursor::TsNode(node) => node
                    .parent()
                    .is_some_and(|parent| kinds.contains(&parent.kind())),

                NodeUnderCursor::CustomNode { .. } => false,
            })
    }
    pub fn before_cursor_matches_kind(&self, kinds: &[&'static str]) -> bool {
        self.node_under_cursor.as_ref().is_some_and(|under_cursor| {
            match under_cursor {
                NodeUnderCursor::TsNode(node) => {
                    let mut current = *node;

                    // move up to the parent until we're at top OR we have a prev sibling
                    while current.prev_sibling().is_none() && current.parent().is_some() {
                        current = current.parent().unwrap();
                    }

                    current
                        .prev_sibling()
                        .is_some_and(|sib| kinds.contains(&sib.kind()))
                }

                NodeUnderCursor::CustomNode {
                    previous_node_kind, ..
                } => previous_node_kind
                    .as_ref()
                    .is_some_and(|k| kinds.contains(&k.as_str())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::context::{TreeSitterContextParams, TreesitterContext, WrappingClause};

    use pgt_test_utils::QueryWithCursorPosition;

    use super::NodeUnderCursor;

    fn get_tree(input: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(tree_sitter_sql::language())
            .expect("Couldn't set language");

        parser.parse(input, None).expect("Unable to parse tree")
    }

    #[test]
    fn identifies_clauses() {
        let test_cases = vec![
            (
                format!(
                    "Select {}* from users;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::Select,
            ),
            (
                format!(
                    "Select * from u{};",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::From,
            ),
            (
                format!(
                    "Select {}* from users where n = 1;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::Select,
            ),
            (
                format!(
                    "Select * from users where {}n = 1;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::Where,
            ),
            (
                format!(
                    "update users set u{} = 1 where n = 2;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::Update,
            ),
            (
                format!(
                    "update users set u = 1 where n{} = 2;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::Where,
            ),
            (
                format!(
                    "delete{} from users;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::Delete,
            ),
            (
                format!(
                    "delete from {}users;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::From,
            ),
            (
                format!(
                    "select name, age, location from public.u{}sers",
                    QueryWithCursorPosition::cursor_marker()
                ),
                WrappingClause::From,
            ),
        ];

        for (query, expected_clause) in test_cases {
            let (position, text) = QueryWithCursorPosition::from(query).get_text_and_position();

            let tree = get_tree(text.as_str());

            let params = TreeSitterContextParams {
                position: (position as u32).into(),
                text: &text,
                tree: &tree,
            };

            let ctx = TreesitterContext::new(params);

            assert_eq!(ctx.wrapping_clause_type, Some(expected_clause));
        }
    }

    #[test]
    fn identifies_schema() {
        let test_cases = vec![
            (
                format!(
                    "Select * from private.u{}",
                    QueryWithCursorPosition::cursor_marker()
                ),
                Some("private"),
            ),
            (
                format!(
                    "Select * from private.u{}sers()",
                    QueryWithCursorPosition::cursor_marker()
                ),
                Some("private"),
            ),
            (
                format!(
                    "Select * from u{}sers",
                    QueryWithCursorPosition::cursor_marker()
                ),
                None,
            ),
            (
                format!(
                    "Select * from u{}sers()",
                    QueryWithCursorPosition::cursor_marker()
                ),
                None,
            ),
        ];

        for (query, expected_schema) in test_cases {
            let (position, text) = QueryWithCursorPosition::from(query).get_text_and_position();

            let tree = get_tree(text.as_str());
            let params = TreeSitterContextParams {
                position: (position as u32).into(),
                text: &text,
                tree: &tree,
            };

            let ctx = TreesitterContext::new(params);

            assert_eq!(
                ctx.schema_or_alias_name,
                expected_schema.map(|f| f.to_string())
            );
        }
    }

    #[test]
    fn identifies_invocation() {
        let test_cases = vec![
            (
                format!(
                    "Select * from u{}sers",
                    QueryWithCursorPosition::cursor_marker()
                ),
                false,
            ),
            (
                format!(
                    "Select * from u{}sers()",
                    QueryWithCursorPosition::cursor_marker()
                ),
                true,
            ),
            (
                format!("Select cool{};", QueryWithCursorPosition::cursor_marker()),
                false,
            ),
            (
                format!("Select cool{}();", QueryWithCursorPosition::cursor_marker()),
                true,
            ),
            (
                format!(
                    "Select upp{}ercase as title from users;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                false,
            ),
            (
                format!(
                    "Select upp{}ercase(name) as title from users;",
                    QueryWithCursorPosition::cursor_marker()
                ),
                true,
            ),
        ];

        for (query, is_invocation) in test_cases {
            let (position, text) = QueryWithCursorPosition::from(query).get_text_and_position();

            let tree = get_tree(text.as_str());
            let params = TreeSitterContextParams {
                position: (position as u32).into(),
                text: text.as_str(),
                tree: &tree,
            };

            let ctx = TreesitterContext::new(params);

            assert_eq!(ctx.is_invocation, is_invocation);
        }
    }

    #[test]
    fn does_not_fail_on_leading_whitespace() {
        let cases = vec![
            format!(
                "{}      select * from",
                QueryWithCursorPosition::cursor_marker()
            ),
            format!(
                " {}      select * from",
                QueryWithCursorPosition::cursor_marker()
            ),
        ];

        for query in cases {
            let (position, text) = QueryWithCursorPosition::from(query).get_text_and_position();

            let tree = get_tree(text.as_str());

            let params = TreeSitterContextParams {
                position: (position as u32).into(),
                text: &text,
                tree: &tree,
            };

            let ctx = TreesitterContext::new(params);

            let node = ctx.node_under_cursor.as_ref().unwrap();

            match node {
                NodeUnderCursor::TsNode(node) => {
                    assert_eq!(ctx.get_ts_node_content(node), Some("select".into()));

                    assert_eq!(
                        ctx.wrapping_clause_type,
                        Some(crate::context::WrappingClause::Select)
                    );
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn does_not_fail_on_trailing_whitespace() {
        let query = format!(
            "select * from   {}",
            QueryWithCursorPosition::cursor_marker()
        );

        let (position, text) = QueryWithCursorPosition::from(query).get_text_and_position();

        let tree = get_tree(text.as_str());

        let params = TreeSitterContextParams {
            position: (position as u32).into(),
            text: &text,
            tree: &tree,
        };

        let ctx = TreesitterContext::new(params);

        let node = ctx.node_under_cursor.as_ref().unwrap();

        match node {
            NodeUnderCursor::TsNode(node) => {
                assert_eq!(ctx.get_ts_node_content(node), Some("from".into()));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn does_not_fail_with_empty_statements() {
        let query = format!("{}", QueryWithCursorPosition::cursor_marker());

        let (position, text) = QueryWithCursorPosition::from(query).get_text_and_position();

        let tree = get_tree(text.as_str());

        let params = TreeSitterContextParams {
            position: (position as u32).into(),
            text: &text,
            tree: &tree,
        };

        let ctx = TreesitterContext::new(params);

        let node = ctx.node_under_cursor.as_ref().unwrap();

        match node {
            NodeUnderCursor::TsNode(node) => {
                assert_eq!(ctx.get_ts_node_content(node), Some("".into()));
                assert_eq!(ctx.wrapping_clause_type, None);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn does_not_fail_on_incomplete_keywords() {
        //  Instead of autocompleting "FROM", we'll assume that the user
        // is selecting a certain column name, such as `frozen_account`.
        let query = format!("select * fro{}", QueryWithCursorPosition::cursor_marker());

        let (position, text) = QueryWithCursorPosition::from(query).get_text_and_position();

        let tree = get_tree(text.as_str());

        let params = TreeSitterContextParams {
            position: (position as u32).into(),
            text: &text,
            tree: &tree,
        };

        let ctx = TreesitterContext::new(params);

        let node = ctx.node_under_cursor.as_ref().unwrap();

        match node {
            NodeUnderCursor::TsNode(node) => {
                assert_eq!(ctx.get_ts_node_content(node), Some("fro".into()));
                assert_eq!(ctx.wrapping_clause_type, Some(WrappingClause::Select));
            }
            _ => unreachable!(),
        }
    }
}
