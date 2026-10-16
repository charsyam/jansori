//! Conservative WHERE-column detection for index-presence hints.
//!
//! Only top-level SELECT predicates are considered. Unresolved columns are
//! omitted instead of being reported as unindexed.

use std::collections::{BTreeMap, HashMap};

use sqlx::{MySqlConnection, Row};

use crate::model::{SqlPatternHint, WhereColumn};

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    String(String),
    Symbol(char),
    Dot,
    Open,
    Close,
    Comma,
    Other,
}

impl Token {
    fn word(&self) -> Option<&str> {
        match self {
            Self::Word(value) => Some(value),
            _ => None,
        }
    }

    fn is(&self, keyword: &str) -> bool {
        self.word()
            .is_some_and(|word| word.eq_ignore_ascii_case(keyword))
    }
}

fn tokens(sql: &str) -> Vec<Token> {
    let chars: Vec<char> = sql.chars().collect();
    let mut result = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '#'
            || (c == '-'
                && chars.get(i + 1) == Some(&'-')
                && chars.get(i + 2).is_some_and(|next| next.is_whitespace()))
        {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
        } else if c == '\'' || c == '"' {
            let quote = c;
            i += 1;
            let mut literal = String::new();
            while i < chars.len() {
                if chars[i] == '\\' {
                    if let Some(next) = chars.get(i + 1) {
                        literal.push(*next);
                    }
                    i = (i + 2).min(chars.len());
                } else if chars[i] == quote {
                    if chars.get(i + 1) == Some(&quote) {
                        literal.push(quote);
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    literal.push(chars[i]);
                    i += 1;
                }
            }
            result.push(Token::String(literal));
        } else if c == '`' {
            i += 1;
            let mut word = String::new();
            while i < chars.len() {
                if chars[i] == '`' {
                    if chars.get(i + 1) == Some(&'`') {
                        word.push('`');
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    word.push(chars[i]);
                    i += 1;
                }
            }
            result.push(Token::Word(word));
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            result.push(Token::Word(chars[start..i].iter().collect()));
        } else {
            result.push(match c {
                '.' => Token::Dot,
                '(' => Token::Open,
                ')' => Token::Close,
                ',' => Token::Comma,
                '!' | '<' | '>' | '=' => Token::Symbol(c),
                _ => Token::Other,
            });
            i += 1;
        }
    }
    result
}

#[derive(Clone, Debug)]
struct TableRef {
    schema: String,
    name: String,
    alias: String,
}

fn reserved(word: &str) -> bool {
    [
        "ON",
        "USING",
        "WHERE",
        "JOIN",
        "INNER",
        "LEFT",
        "RIGHT",
        "FULL",
        "OUTER",
        "CROSS",
        "STRAIGHT_JOIN",
        "GROUP",
        "ORDER",
        "LIMIT",
        "HAVING",
        "UNION",
        "FOR",
        "AND",
        "OR",
        "NOT",
        "IN",
        "IS",
        "NULL",
        "TRUE",
        "FALSE",
        "BETWEEN",
        "LIKE",
        "REGEXP",
        "EXISTS",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "AS",
        "ASC",
        "DESC",
        "SELECT",
        "FROM",
        "DISTINCT",
    ]
    .iter()
    .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

fn table_refs(tokens: &[Token], default_schema: &str) -> Vec<TableRef> {
    let mut result = Vec::new();
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::Open => depth += 1,
            Token::Close => depth = (depth - 1).max(0),
            _ => {}
        }
        if depth != 0 || !(token.is("FROM") || token.is("JOIN")) {
            continue;
        }
        let Some(first) = tokens.get(i + 1).and_then(Token::word) else {
            continue;
        };
        let (schema, name, mut next) = if tokens.get(i + 2) == Some(&Token::Dot) {
            let Some(table) = tokens.get(i + 3).and_then(Token::word) else {
                continue;
            };
            (first.to_owned(), table.to_owned(), i + 4)
        } else {
            (default_schema.to_owned(), first.to_owned(), i + 2)
        };
        if tokens.get(next).is_some_and(|token| token.is("AS")) {
            next += 1;
        }
        let alias = tokens
            .get(next)
            .and_then(Token::word)
            .filter(|word| !reserved(word))
            .unwrap_or(&name)
            .to_owned();
        result.push(TableRef {
            schema,
            name,
            alias,
        });
    }
    result
}

fn is_boolean_literal(token: Option<&Token>) -> bool {
    token.is_some_and(|token| token.is("TRUE") || token.is("FALSE"))
}

fn compares_with_boolean(tokens: &[Token], start: usize, next: usize) -> bool {
    let after = if tokens.get(next).is_some_and(|token| token.is("IS")) {
        next + 1 + usize::from(tokens.get(next + 1).is_some_and(|token| token.is("NOT")))
    } else if matches!(tokens.get(next), Some(Token::Symbol(_))) {
        next + 1 + usize::from(matches!(tokens.get(next + 1), Some(Token::Symbol(_))))
    } else {
        next
    };
    (after != next && is_boolean_literal(tokens.get(after)))
        || (start >= 2
            && matches!(tokens[start - 1], Token::Symbol(_))
            && is_boolean_literal(tokens.get(start - 2)))
}

fn where_span(tokens: &[Token]) -> Option<(usize, usize)> {
    let mut depth = 0;
    let mut start = None;
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::Open => depth += 1,
            Token::Close => depth = (depth - 1).max(0),
            _ => {}
        }
        if depth == 0 && token.is("WHERE") {
            start = Some(i + 1);
            break;
        }
    }
    let start = start?;
    let mut end = tokens.len();
    depth = 0;
    for (offset, token) in tokens[start..].iter().enumerate() {
        match token {
            Token::Open => depth += 1,
            Token::Close => depth = (depth - 1).max(0),
            _ => {}
        }
        if depth == 0
            && ["GROUP", "ORDER", "LIMIT", "HAVING", "UNION", "FOR"]
                .iter()
                .any(|word| token.is(word))
        {
            end = start + offset;
            break;
        }
    }
    Some((start, end))
}

fn where_refs(tokens: &[Token]) -> Vec<(Option<String>, String)> {
    let mut result = Vec::new();
    let Some((start, end)) = where_span(tokens) else {
        return result;
    };
    let mut i = start;
    while i < end {
        // Subquery names belong to their own scope; leave them unclassified.
        if tokens[i] == Token::Open && tokens.get(i + 1).is_some_and(|token| token.is("SELECT")) {
            let mut nested = 1;
            i += 1;
            while i < end && nested > 0 {
                if tokens[i] == Token::Open {
                    nested += 1;
                }
                if tokens[i] == Token::Close {
                    nested -= 1;
                }
                i += 1;
            }
            continue;
        }
        let Some(word) = tokens[i].word() else {
            i += 1;
            continue;
        };
        if tokens.get(i + 1) == Some(&Token::Dot) {
            if let Some(column) = tokens.get(i + 2).and_then(Token::word) {
                if tokens.get(i + 3) == Some(&Token::Dot) {
                    if let Some(last) = tokens.get(i + 4).and_then(Token::word) {
                        if !compares_with_boolean(tokens, i, i + 5) {
                            result.push((Some(column.to_owned()), last.to_owned()));
                        }
                        i += 5;
                        continue;
                    }
                }
                if !compares_with_boolean(tokens, i, i + 3) {
                    result.push((Some(word.to_owned()), column.to_owned()));
                }
                i += 3;
                continue;
            }
        }
        if !reserved(word)
            && !word
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit())
            && tokens.get(i + 1) != Some(&Token::Open)
            && (i == 0 || tokens[i - 1] != Token::Dot)
        {
            if !compares_with_boolean(tokens, i, i + 1) {
                result.push((None, word.to_owned()));
            }
        }
        i += 1;
    }
    result
}

pub fn pattern_hints(sql: &str) -> Vec<SqlPatternHint> {
    let tokens = tokens(sql);
    let Some((start, end)) = where_span(&tokens) else {
        return Vec::new();
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut i = start;
    while i < end {
        // Nested SELECTs have their own WHERE clause and EXPLAIN steps.
        if tokens[i] == Token::Open && tokens.get(i + 1).is_some_and(|token| token.is("SELECT")) {
            let mut nested = 1;
            i += 1;
            while i < end && nested > 0 {
                if tokens[i] == Token::Open {
                    nested += 1;
                } else if tokens[i] == Token::Close {
                    nested -= 1;
                }
                i += 1;
            }
            continue;
        }
        let hint = if tokens[i].is("NOT") {
            let next = tokens.get(i + 1);
            if next.is_some_and(|token| token.is("IN")) {
                Some("not_in")
            } else if next.is_some_and(|token| token.is("LIKE")) {
                Some("not_like")
            } else if i > start
                && tokens[i - 1].is("IS")
                && next
                    .is_some_and(|token| token.is("NULL") || token.is("TRUE") || token.is("FALSE"))
            {
                None
            } else if next.is_some_and(|token| token.is("EXISTS")) {
                None
            } else {
                Some("not")
            }
        } else if matches!(
            (tokens.get(i), tokens.get(i + 1)),
            (Some(Token::Symbol('!')), Some(Token::Symbol('=')))
                | (Some(Token::Symbol('<')), Some(Token::Symbol('>')))
        ) {
            Some("not_equal")
        } else if tokens[i].is("LIKE")
            && matches!(tokens.get(i + 1), Some(Token::String(value)) if value.starts_with('%') || value.starts_with('_'))
        {
            Some("leading_wildcard_like")
        } else {
            None
        };
        if let Some(hint) = hint {
            *counts.entry(hint).or_default() += 1;
        }
        i += 1;
    }
    counts
        .into_iter()
        .map(|(kind, count)| SqlPatternHint {
            kind: kind.to_owned(),
            count,
        })
        .collect()
}

pub async fn inspect(
    connection: &mut MySqlConnection,
    sql: &str,
    default_schema: &str,
) -> Result<Vec<WhereColumn>, sqlx::Error> {
    let tokens = tokens(sql);
    let tables = table_refs(&tokens, default_schema);
    if tables.is_empty() {
        return Ok(Vec::new());
    }
    let references = where_refs(&tokens);
    if references.is_empty() {
        return Ok(Vec::new());
    }
    // One metadata query per referenced table. The join includes columns with
    // no index, which lets us distinguish "not indexed" from "not a column".
    let mut columns: Vec<HashMap<String, (String, Vec<String>)>> = Vec::new();
    for table in &tables {
        let rows = sqlx::query(
            "SELECT c.COLUMN_NAME, s.INDEX_NAME FROM information_schema.COLUMNS c \
             LEFT JOIN information_schema.STATISTICS s ON s.TABLE_SCHEMA = c.TABLE_SCHEMA \
             AND s.TABLE_NAME = c.TABLE_NAME AND s.COLUMN_NAME = c.COLUMN_NAME \
             WHERE c.TABLE_SCHEMA = ? AND c.TABLE_NAME = ?",
        )
        .bind(&table.schema)
        .bind(&table.name)
        .fetch_all(&mut *connection)
        .await?;
        let mut mapped: HashMap<String, (String, Vec<String>)> = HashMap::new();
        for row in rows {
            let column: String = row.try_get(0)?;
            let index: Option<String> = row.try_get(1)?;
            let entry = mapped
                .entry(column.to_lowercase())
                .or_insert_with(|| (column, Vec::new()));
            if let Some(index) = index {
                if !entry.1.contains(&index) {
                    entry.1.push(index);
                }
            }
        }
        columns.push(mapped);
    }
    let mut found = BTreeMap::new();
    for (owner, column) in references {
        let candidate = if let Some(owner) = owner {
            let matches: Vec<_> = tables
                .iter()
                .enumerate()
                .filter(|(_, table)| {
                    table.alias.eq_ignore_ascii_case(&owner)
                        || table.name.eq_ignore_ascii_case(&owner)
                })
                .filter(|(i, _)| columns[*i].contains_key(&column.to_lowercase()))
                .map(|(i, _)| i)
                .collect();
            (matches.len() == 1).then(|| matches[0])
        } else {
            let matches: Vec<_> = columns
                .iter()
                .enumerate()
                .filter(|(_, mapped)| mapped.contains_key(&column.to_lowercase()))
                .map(|(i, _)| i)
                .collect();
            (matches.len() == 1).then(|| matches[0])
        };
        let Some(i) = candidate else {
            continue;
        };
        let Some((real_name, index_names)) = columns[i].get(&column.to_lowercase()) else {
            continue;
        };
        found.insert(
            (tables[i].name.to_lowercase(), real_name.to_lowercase()),
            WhereColumn {
                table: tables[i].name.clone(),
                column: real_name.clone(),
                index_names: index_names.clone(),
            },
        );
    }
    Ok(found.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_where_columns_and_resolves_table_aliases() {
        let sql = "SELECT p.id FROM posts AS p JOIN users u ON u.id=p.user_id WHERE p.status='active' AND u.id > 1 AND p.deleted_at IS NULL ORDER BY p.id";
        let tokens = tokens(sql);
        let tables = table_refs(&tokens, "app");
        assert_eq!(
            tables
                .iter()
                .map(|table| table.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["p", "u"]
        );
        assert_eq!(
            where_refs(&tokens),
            vec![
                (Some("p".into()), "status".into()),
                (Some("u".into()), "id".into()),
                (Some("p".into()), "deleted_at".into())
            ]
        );
    }

    #[test]
    fn ignores_strings_comments_and_nested_selects() {
        let sql = "SELECT * FROM posts p WHERE p.title='where x.no_index' /* where p.fake=1 */ AND p.id IN (SELECT post_id FROM logs WHERE secret=1) AND p.status = 1";
        assert_eq!(
            where_refs(&tokens(sql)),
            vec![
                (Some("p".into()), "title".into()),
                (Some("p".into()), "id".into()),
                (Some("p".into()), "status".into())
            ]
        );
    }

    #[test]
    fn ignores_boolean_comparisons_but_keeps_other_predicates() {
        let sql = "SELECT * FROM posts p WHERE p.visible = TRUE AND p.deleted IS NOT FALSE AND FALSE = p.archived AND p.status = 'published' AND p.id = 42";
        assert_eq!(
            where_refs(&tokens(sql)),
            vec![
                (Some("p".into()), "status".into()),
                (Some("p".into()), "id".into()),
            ]
        );
    }

    #[test]
    fn reports_reviewable_where_patterns_without_sql_text_false_positives() {
        let sql = "SELECT * FROM posts WHERE id NOT IN (1, 2) AND status NOT IN (3, 4) AND NOT (archived = 1) AND title LIKE '%foo' AND body LIKE 'foo%' AND score != 1 AND rank <> 2 AND deleted_at IS NOT NULL AND note = 'NOT IN' /* NOT LIKE */ ORDER BY id";
        assert_eq!(
            pattern_hints(sql),
            vec![
                SqlPatternHint {
                    kind: "leading_wildcard_like".into(),
                    count: 1
                },
                SqlPatternHint {
                    kind: "not".into(),
                    count: 1
                },
                SqlPatternHint {
                    kind: "not_equal".into(),
                    count: 2
                },
                SqlPatternHint {
                    kind: "not_in".into(),
                    count: 2
                },
            ]
        );
    }

    #[test]
    fn skips_patterns_inside_nested_selects() {
        let sql = "SELECT * FROM posts WHERE id IN (SELECT post_id FROM logs WHERE level NOT IN (1, 2)) AND title NOT LIKE '%tmp'";
        assert_eq!(
            pattern_hints(sql),
            vec![
                SqlPatternHint {
                    kind: "leading_wildcard_like".into(),
                    count: 1
                },
                SqlPatternHint {
                    kind: "not_like".into(),
                    count: 1
                },
            ]
        );
    }
}
