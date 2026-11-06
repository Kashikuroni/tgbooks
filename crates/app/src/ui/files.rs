//! Shared display rules, never disk or Telegram queries.

use chrono::{DateTime, Local};
use tgbooks_core::{BookFormat, DownloadRecord};

pub fn count(value: usize, one: &str, few: &str, many: &str) -> String {
    let word = if (11..=14).contains(&(value % 100)) {
        many
    } else {
        match value % 10 {
            1 => one,
            2..=4 => few,
            _ => many,
        }
    };
    format!("{value} {word}")
}

pub fn matches(name: &str, source: Option<&str>, query: &str, format: Option<BookFormat>) -> bool {
    let query = query.trim().to_lowercase();
    (query.is_empty()
        || name.to_lowercase().contains(&query)
        || source.is_some_and(|source| source.to_lowercase().contains(&query)))
        && format.is_none_or(|format| BookFormat::from_file_name(name) == Some(format))
}

pub fn source(record: &DownloadRecord) -> String {
    record
        .source_title
        .clone()
        .unwrap_or_else(|| format!("Канал {}", record.key.channel_id))
}

pub fn date(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "—".into();
    }
    DateTime::from_timestamp(timestamp, 0)
        .map(|time| {
            time.with_timezone(&Local)
                .format("%d.%m.%Y, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

pub fn folder(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(folder_path)
        .unwrap_or_else(|| "—".into())
}

pub fn folder_path(path: &std::path::Path) -> String {
    let parts: Vec<_> = path
        .components()
        .rev()
        .take(2)
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.into_iter().rev().collect::<Vec<_>>().join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_search_matches_unicode_names_and_sources_with_format_intersection() {
        assert!(matches(
            "Толстой 世界.EPUB",
            Some("Книжная полка"),
            "ТОЛСТОЙ",
            Some(BookFormat::Epub)
        ));
        assert!(matches("世界.zip", Some("Книжная полка"), "полка", None));
        assert!(!matches("世界.zip", None, "полка", None));
        assert!(!matches("book.pdf", None, "", Some(BookFormat::Zip)));
        assert_eq!(date(0), "—");
        assert_eq!(date(i64::MAX), "—");
    }
}
