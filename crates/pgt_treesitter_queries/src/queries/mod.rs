mod insert_columns;
mod parameters;
mod relations;
mod select_columns;
mod table_aliases;
mod where_columns;

pub use insert_columns::*;
pub use parameters::*;
pub use relations::*;
pub use select_columns::*;
pub use table_aliases::*;
pub use where_columns::*;

#[derive(Debug)]
pub enum QueryResult<'a> {
    Relation(RelationMatch<'a>),
    Parameter(ParameterMatch<'a>),
    TableAliases(TableAliasMatch<'a>),
    SelectClauseColumns(SelectColumnMatch<'a>),
    InsertClauseColumns(InsertColumnMatch<'a>),
    WhereClauseColumns(WhereColumnMatch<'a>),
}

impl QueryResult<'_> {
    pub fn within_range(&self, range: &tree_sitter::Range) -> bool {
        match self {
            QueryResult::Relation(rm) => {
                let start = match rm.schema {
                    Some(s) => s.start_position(),
                    None => rm.table.start_position(),
                };

                let end = rm.table.end_position();

                start >= range.start_point && end <= range.end_point
            }
            Self::Parameter(pm) => {
                let node_range = pm.node.range();

                node_range.start_point >= range.start_point
                    && node_range.end_point <= range.end_point
            }
            QueryResult::TableAliases(m) => {
                let start = m.table.start_position();
                let end = m.alias.end_position();
                start >= range.start_point && end <= range.end_point
            }
            Self::SelectClauseColumns(cm) => {
                let start = match cm.alias {
                    Some(n) => n.start_position(),
                    None => cm.column.start_position(),
                };

                let end = cm.column.end_position();

                start >= range.start_point && end <= range.end_point
            }
            Self::WhereClauseColumns(cm) => {
                let start = match cm.alias {
                    Some(n) => n.start_position(),
                    None => cm.column.start_position(),
                };

                let end = cm.column.end_position();

                start >= range.start_point && end <= range.end_point
            }
            Self::InsertClauseColumns(cm) => {
                let start = cm.column.start_position();
                let end = cm.column.end_position();
                start >= range.start_point && end <= range.end_point
            }
        }
    }
}

// This trait enforces that for any `Self` that implements `Query`,
// its &Self must implement TryFrom<&QueryResult>
pub(crate) trait QueryTryFrom<'a>: Sized {
    type Ref: for<'any> TryFrom<&'a QueryResult<'a>, Error = String>;
}

pub(crate) trait Query<'a>: QueryTryFrom<'a> {
    fn execute(root_node: tree_sitter::Node<'a>, stmt: &'a str) -> Vec<crate::QueryResult<'a>>;
}
