use super::error::{BindError, BindResult};
use crate::{catalog, databox::DataType};

#[derive(Debug, Clone)]
pub struct ScopeColumn {
    pub qualifier: Option<String>,
    pub column_name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub index: usize,
    pub catalog_column: Option<catalog::Column>,
}

#[derive(Debug, Default, Clone)]
pub struct Scope {
    pub columns: Vec<ScopeColumn>,
}

impl Scope {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_table(
        &mut self,
        qualifier: Option<String>,
        columns: &[catalog::Column],
        nullable: bool,
    ) {
        for col in columns {
            let index = self.columns.len();
            self.columns.push(ScopeColumn {
                qualifier: qualifier.clone(),
                column_name: col.name.to_string(),
                data_type: col.type_id,
                nullable: col.nullable || nullable,
                index,
                catalog_column: Some(col.clone()),
            });
        }
    }

    pub fn add_derived(&mut self, name: String, data_type: DataType, nullable: bool) {
        let index = self.columns.len();
        self.columns.push(ScopeColumn {
            qualifier: None,
            column_name: name,
            data_type,
            nullable,
            index,
            catalog_column: None,
        });
    }

    pub fn resolve(&self, qualifier: Option<&str>, column_name: &str) -> BindResult<&ScopeColumn> {
        let matches: Vec<&ScopeColumn> = self
            .columns
            .iter()
            .filter(|sc| {
                sc.column_name.eq_ignore_ascii_case(column_name)
                    && match qualifier {
                        Some(q) => sc
                            .qualifier
                            .as_deref()
                            .map(|sq| sq.eq_ignore_ascii_case(q))
                            .unwrap_or(false),
                        None => true,
                    }
            })
            .collect();

        match matches.len() {
            1 => Ok(matches[0]),
            0 => Err(BindError::UnknownColumn(
                qualifier
                    .map(|q| format!("{q}.{column_name}"))
                    .unwrap_or_else(|| column_name.to_string()),
            )),
            _ => Err(BindError::AmbiguousColumn(column_name.to_string())),
        }
    }

    pub fn all_columns_for_qualifier(&self, qualifier: &str) -> Vec<&ScopeColumn> {
        self.columns
            .iter()
            .filter(|sc| {
                sc.qualifier
                    .as_deref()
                    .map(|q| q.eq_ignore_ascii_case(qualifier))
                    .unwrap_or(false)
            })
            .collect()
    }
}
