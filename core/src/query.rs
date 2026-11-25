use crate::{
    Mosh, Object, now, object,
    organization::{AUTHORITY, VISIBLE, tag_name},
    prefix,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Size,
    Path,
    NameDesc,
    ModifiedAsc,
    SizeAsc,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SearchOptions {
    pub limit: usize,
    pub offset: usize,
    pub sort: Sort,
    pub parent: Option<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 200,
            offset: 0,
            sort: Sort::Name,
            parent: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Term(String),
    And,
    Or,
    Not,
    Open,
    Close,
}

fn tokenize(query: &str) -> Result<Vec<Token>> {
    ensure!(query.len() <= 8192, "query is too long");
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut had_quotes = false;
    let mut chars = query.chars().peekable();
    let flush = |word: &mut String, quoted: &mut bool, tokens: &mut Vec<Token>| {
        if !word.is_empty() || *quoted {
            let token = if *quoted {
                Token::Term(std::mem::take(word))
            } else {
                match word.as_str() {
                    "AND" => Token::And,
                    "OR" => Token::Or,
                    "NOT" => Token::Not,
                    _ => Token::Term(std::mem::take(word)),
                }
            };
            word.clear();
            *quoted = false;
            tokens.push(token);
        }
    };
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                quoted = !quoted;
                had_quotes = true;
            }
            '\\' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                word.push('"');
            }
            '(' | ')' if !quoted => {
                flush(&mut word, &mut had_quotes, &mut tokens);
                tokens.push(if ch == '(' { Token::Open } else { Token::Close });
            }
            ch if !quoted && ch.is_whitespace() => flush(&mut word, &mut had_quotes, &mut tokens),
            ch => word.push(ch),
        }
    }
    ensure!(!quoted, "unclosed quoted search value");
    flush(&mut word, &mut had_quotes, &mut tokens);
    ensure!(tokens.len() <= 96, "query contains too many terms");
    Ok(tokens)
}

struct Parser {
    tokens: std::vec::IntoIter<Token>,
    next: Option<Token>,
    values: Vec<Value>,
}

impl Parser {
    fn advance(&mut self) -> Option<Token> {
        std::mem::replace(&mut self.next, self.tokens.next())
    }

    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.values.len())
    }

    fn or(&mut self) -> Result<String> {
        let mut expressions = vec![self.and()?];
        while self.next == Some(Token::Or) {
            self.advance();
            expressions.push(self.and()?);
        }
        Ok(expressions.join(" OR "))
    }

    fn and(&mut self) -> Result<String> {
        let mut expressions = Vec::new();
        let mut direct = Vec::new();
        loop {
            if let Some(Token::Term(term)) = &self.next
                && let Some(tag) = term.strip_prefix("direct-tag:")
            {
                let tag = tag_name(tag)?;
                self.advance();
                direct.push(self.bind(tag));
            } else {
                expressions.push(self.atom()?);
            }
            match self.next {
                Some(Token::And) => {
                    self.advance();
                }
                Some(Token::Not | Token::Open | Token::Term(_)) => {}
                _ => break,
            }
        }
        // Bound the join width for long direct-tag conjunctions.
        expressions.extend(direct.chunks(8).map(direct_intersection));
        Ok(expressions.join(" AND "))
    }

    fn atom(&mut self) -> Result<String> {
        match self.advance() {
            Some(Token::Not) => Ok(format!("NOT ({})", self.atom()?)),
            Some(Token::Open) => {
                let expression = self.or()?;
                ensure!(
                    self.advance() == Some(Token::Close),
                    "missing closing parenthesis"
                );
                Ok(format!("({expression})"))
            }
            Some(Token::Term(term)) => self.term(&term),
            _ => bail!("expected a search term"),
        }
    }

    fn term(&mut self, term: &str) -> Result<String> {
        let (field, value) = term.split_once(':').unwrap_or(("", term));
        ensure!(!value.is_empty(), "a search value is empty");
        let clause = match field {
            "tag" | "direct-tag" | "inherited-tag" | "suggested-tag" => {
                let id = self.bind(tag_name(value)?);
                let suggested = u8::from(field == "suggested-tag");
                let direct = format!("SELECT k.object FROM tag_blocks b JOIN json_each(mosh_block_slots(b.packed)) p
                    JOIN object_keys k ON k.key=b.block*4096+p.value WHERE b.suggested={suggested}
                    AND b.tag IN(SELECT id FROM tag_dictionary WHERE name={id} UNION SELECT tag FROM tag_aliases WHERE alias={id})");
                if matches!(field, "tag" | "inherited-tag") {
                    let selection = if field == "tag" {
                        "SELECT id FROM matched"
                    } else {
                        "SELECT o2.id FROM objects o2 JOIN matched m2 ON o2.parent=m2.id WHERE o2.missing=0"
                    };
                    format!("o.id IN(WITH RECURSIVE matched(id) AS ({direct}
                        UNION SELECT child.id FROM objects child JOIN matched m ON child.parent=m.id WHERE child.missing=0)
                        {selection})")
                } else {
                    format!("o.id IN({direct})")
                }
            }
            "name" => format!("o.name={} COLLATE NOCASE", self.bind(value.to_owned())),
            "name-contains" => format!(
                "instr(lower(o.name),lower({}))>0",
                self.bind(value.to_owned())
            ),
            "path" => format!(
                "instr(lower(o.path),lower({}))>0",
                self.bind(value.to_owned())
            ),
            "ext" | "extension" => format!(
                "o.extension={}",
                self.bind(value.trim_start_matches('.').to_lowercase())
            ),
            "kind" => {
                let kind = match value {
                    "folder" | "coll" => "coll",
                    "file" | "marriage" => value,
                    _ => bail!("unknown object kind"),
                };
                format!("o.kind={}", self.bind(kind.to_owned()))
            }
            "virtual" => {
                ensure!(
                    matches!(value, "true" | "false"),
                    "virtual expects true or false"
                );
                if value == "true" {
                    "o.path IS NULL".to_owned()
                } else {
                    "o.path IS NOT NULL".to_owned()
                }
            }
            "type" if value == "source-code" => self.term("tag:source-code")?,
            "type" | "mime" => {
                let value = value.trim_end_matches('*');
                let id = self.bind(value.to_owned());
                format!(
                    "(o.mime={id} OR (substr(o.mime,1,length({id}))={id} AND
                    (substr(o.mime,length({id})+1,1)='/' OR substr({id},-1)='/')))"
                )
            }
            "in" => {
                let (low, high) = prefix(value);
                let low = self.bind(low);
                let high = self.bind(high);
                format!("(o.path>={low} AND o.path<{high})")
            }
            "modified" | "created" | "accessed" | "changed" | "git-age" => {
                let column = if field == "git-age" {
                    "git_committed"
                } else {
                    field
                };
                let (op, value) = comparison(value);
                let op = if age(value).is_some() { flip(op) } else { op };
                if let Some((start, end)) = value.split_once("..") {
                    let start = self.bind(date(start)?);
                    let end = self.bind(date(end)?);
                    format!("o.{column} BETWEEN {start} AND {end}")
                } else {
                    format!("o.{column}{op}{}", self.bind(date(value)?))
                }
            }
            "size" | "allocated" => {
                let (op, value) = comparison(value);
                format!("o.{field}{op}{}", self.bind(bytes(value)?))
            }
            "duplicate" => {
                ensure!(
                    matches!(value, "true" | "false"),
                    "duplicate expects true or false"
                );
                let members = format!("o.hash IS NOT NULL AND EXISTS(SELECT 1 FROM objects d WHERE d.hash=o.hash
                    AND d.missing=0 AND d.id<>o.id AND (o.native_id IS NULL OR d.native_id IS NOT o.native_id)
                    AND {})",VISIBLE.replace("o.id","d.id").replace("o.path","d.path"));
                if value == "true" {
                    format!("({members})")
                } else {
                    format!("NOT ({members})")
                }
            }
            "hash" => format!("o.hash={}", self.bind(value.to_lowercase())),
            "repo" => format!(
                "o.git_root IN(SELECT path FROM repositories WHERE name={} COLLATE NOCASE)",
                self.bind(value.to_owned())
            ),
            "branch" => format!(
                "o.git_root IN(SELECT path FROM repositories WHERE branch={})",
                self.bind(value.to_owned())
            ),
            "git-status" => format!("o.git_status={}", self.bind(value.to_lowercase())),
            "git-author" => format!(
                "instr(lower(o.git_author),lower({}))>0",
                self.bind(value.to_owned())
            ),
            "owner" => format!(
                "o.id IN(SELECT object FROM owners WHERE person={})",
                self.bind(value.to_owned())
            ),
            "marriage" | "logical-parent" => {
                let collection = format!(
                    "SELECT c.id FROM objects c WHERE c.missing=0 AND (c.id=?VALUE OR c.name=?VALUE COLLATE NOCASE) AND {}",
                    VISIBLE.replace("o.id", "c.id").replace("o.path", "c.path")
                );
                let collection = collection.replace("?VALUE", &self.bind(value.to_owned()));
                let kind = if field == "marriage" {
                    "reference"
                } else {
                    "parent"
                };
                format!(
                    "o.id IN(SELECT target FROM edges WHERE kind='{kind}' AND container IN({collection}))"
                )
            }
            "content" | "paint" | "text" | "" => {
                let phrase = format!("\"{}\"", value.replace('"', "\"\""));
                let mut clauses = Vec::new();
                if field != "content" {
                    let fts = self.bind(if field == "paint" {
                        format!("paint:{phrase}")
                    } else {
                        phrase.clone()
                    });
                    clauses.push(format!("o.id IN(SELECT k.object FROM ledger JOIN object_keys k ON k.key=ledger.rowid WHERE ledger MATCH {fts})"));
                }
                if field.is_empty() {
                    clauses.push(format!(
                        "instr(lower(o.name),lower({}))>0",
                        self.bind(value.to_owned())
                    ));
                }
                if field != "paint" {
                    let term = self.bind(phrase);
                    clauses.push(format!("o.id IN(SELECT k.object FROM text_terms JOIN object_keys k ON k.key=text_terms.rowid
                        WHERE text_terms MATCH {term})"));
                    clauses.push(format!("CASE WHEN o.missing=0 AND o.kind='file' AND o.reparse=0 AND o.path IS NOT NULL
                        AND mosh_text_supported(o.extension) AND {VISIBLE} AND NOT EXISTS(
                            SELECT 1 FROM object_keys k JOIN text_cached c ON c.key=k.key WHERE k.object=o.id
                        ) THEN mosh_live_match(o.path,o.extension,o.native_id,o.created,{term}) ELSE 0 END"));
                }
                format!("({})", clauses.join(" OR "))
            }
            _ => bail!("unknown search field {field}"),
        };
        Ok(clause)
    }
}

fn direct_intersection(tags: &[String]) -> String {
    let resolved = |id: &str| {
        format!(
            "SELECT id FROM tag_dictionary WHERE name={id} UNION SELECT tag FROM tag_aliases WHERE alias={id}"
        )
    };
    let mut joins = String::new();
    let mut packed = vec!["b0.packed".to_owned()];
    for (index, tag) in tags.iter().enumerate().skip(1) {
        joins.push_str(&format!(
            " JOIN tag_blocks b{index} ON b{index}.block=b0.block
            AND b{index}.suggested=0 AND b{index}.tag IN({})",
            resolved(tag)
        ));
        packed.push(format!("b{index}.packed"));
    }
    format!(
        "o.id IN(SELECT k.object FROM tag_blocks b0 {joins}
        JOIN json_each(mosh_common_slots({})) p JOIN object_keys k ON k.key=b0.block*4096+p.value
        WHERE b0.suggested=0 AND b0.tag IN({}))",
        packed.join(","),
        resolved(&tags[0])
    )
}

fn comparison(input: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = input.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("=", input)
}

fn flip(op: &str) -> &str {
    match op {
        ">" => "<",
        "<" => ">",
        ">=" => "<=",
        "<=" => ">=",
        _ => op,
    }
}

fn age(input: &str) -> Option<i64> {
    let unit = match input.chars().last()? {
        'h' => 3_600,
        'd' => 86_400,
        'w' => 604_800,
        _ => return None,
    };
    let amount = input[..input.len() - 1].parse::<i64>().ok()?;
    (amount >= 0)
        .then_some(amount)?
        .checked_mul(unit)?
        .checked_mul(1000)
}

fn date(input: &str) -> Result<i64> {
    if let Some(ms) = age(input) {
        return now().checked_sub(ms).context("age out of range");
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(input) {
        return Ok(parsed.timestamp_millis());
    }
    Ok(chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d")
        .context("use YYYY-MM-DD, RFC3339, or an age like 30d")?
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis())
}

fn bytes(input: &str) -> Result<i64> {
    let index = input
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(input.len());
    let n: f64 = input[..index].parse().context("invalid size")?;
    let multiplier = match input[index..].to_ascii_uppercase().as_str() {
        "" | "B" => 1f64,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KIB" => 1024f64,
        "MIB" => 1048576f64,
        "GIB" => 1073741824f64,
        "TIB" => 1099511627776f64,
        _ => bail!("invalid size unit"),
    };
    let bytes = n * multiplier;
    ensure!(
        bytes.is_finite() && (0f64..i64::MAX as f64).contains(&bytes),
        "size out of range"
    );
    Ok(bytes.round() as i64)
}

fn compile(query: &str, actor: &str) -> Result<(String, Parser)> {
    let mut parser = Parser {
        tokens: tokenize(query)?.into_iter(),
        next: None,
        values: vec![
            actor.to_owned().into(),
            std::path::MAIN_SEPARATOR_STR.to_owned().into(),
        ],
    };
    parser.advance();
    let predicate = if parser.next.is_none() {
        "1".to_owned()
    } else {
        parser.or()?
    };
    ensure!(
        parser.next.is_none(),
        "unexpected search operator or closing parenthesis"
    );
    Ok((predicate, parser))
}

pub(crate) fn validate(query: &str) -> Result<()> {
    compile(query, "local").map(|_| ())
}

impl Mosh {
    pub fn search(&self, query: &str, options: &SearchOptions) -> Result<Vec<Object>> {
        let (predicate, mut parser) = compile(query, &self.actor)?;
        let order = match options.sort {
            Sort::Name => "o.name COLLATE NOCASE,o.id",
            Sort::NameDesc => "o.name COLLATE NOCASE DESC,o.id",
            Sort::Modified => "o.modified DESC,o.id",
            Sort::ModifiedAsc => "o.modified,o.id",
            Sort::Size => "o.size DESC,o.id",
            Sort::SizeAsc => "o.size,o.id",
            Sort::Path => "o.path,o.id",
        };
        let mut scope = String::new();
        if let Some(parent) = &options.parent {
            let selected = self.get(parent)?;
            let id = parser.bind(selected.id.clone());
            scope = format!(
                "AND (o.parent={id} OR o.id IN(SELECT target FROM edges WHERE container={id} AND kind<>'related')"
            );
            if selected.kind == "marriage" {
                let mut statement = self.db.prepare("SELECT query FROM collection_queries WHERE container=? ORDER BY query LIMIT 40")?;
                for expression in statement.query_map([&selected.id], |r| r.get::<_, String>(0))? {
                    let (predicate, dynamic) = compile(&expression?, &self.actor)?;
                    let start = parser.values.len();
                    let mapped = remap(&predicate, start);
                    parser.values.extend(dynamic.values.into_iter().skip(2));
                    scope.push_str(&format!(" OR ({mapped})"));
                }
            }
            scope.push(')');
        }
        let limit = parser.bind(options.limit.clamp(1, 2000) as i64);
        let offset = parser.bind(options.offset.min(i64::MAX as usize) as i64);
        let sql = format!(
            "{AUTHORITY} SELECT o.* FROM objects o WHERE o.missing=0 AND {VISIBLE}
            AND ({predicate}) {scope} ORDER BY {order} LIMIT {limit} OFFSET {offset}"
        );
        let mut statement = self.db.prepare(&sql)?;
        let results = statement.query_map(params_from_iter(parser.values), object)?;
        Ok(results.collect::<rusqlite::Result<_>>()?)
    }
}

fn remap(sql: &str, base: usize) -> String {
    let mut output = String::new();
    let mut characters = sql.chars().peekable();
    while let Some(ch) = characters.next() {
        output.push(ch);
        if ch == '?' {
            let digits: String = std::iter::from_fn(|| {
                if characters.peek().is_some_and(char::is_ascii_digit) {
                    characters.next()
                } else {
                    None
                }
            })
            .collect();
            let n = digits.parse::<usize>().unwrap();
            output.push_str(&(if n <= 2 { n } else { n + base - 2 }).to_string());
        }
    }
    output
}
