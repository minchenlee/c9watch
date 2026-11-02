use c9watch_core::session::{
    extract_messages, parse_last_n_entries, parse_sessions_index, MessageType,
};
use std::env;
use std::path::PathBuf;

#[test]
fn test_parse_real_sessions_index() {
    // This test will only run if the Claude directory exists
    let home = match env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            println!("HOME env var not set, skipping test");
            return;
        }
    };

    let index_path = PathBuf::from(&home)
        .join(".claude/projects/-Users-vincent-m-lee--claude/sessions-index.json");

    if !index_path.exists() {
        println!(
            "Sessions index not found at {:?}, skipping test",
            index_path
        );
        return;
    }

    let result = parse_sessions_index(&index_path);
    assert!(
        result.is_ok(),
        "Failed to parse sessions-index.json: {:?}",
        result.err()
    );

    let index = result.unwrap();
    assert_eq!(index.version, 1);
    assert!(
        !index.entries.is_empty(),
        "Sessions index should have entries"
    );

    println!("Successfully parsed {} sessions", index.entries.len());
}

#[test]
fn test_parse_real_jsonl_file() {
    let home = match env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            println!("HOME env var not set, skipping test");
            return;
        }
    };

    let index_path = PathBuf::from(&home)
        .join(".claude/projects/-Users-vincent-m-lee--claude/sessions-index.json");

    if !index_path.exists() {
        println!("Sessions index not found, skipping test");
        return;
    }

    let index = match parse_sessions_index(&index_path) {
        Ok(i) => i,
        Err(_) => {
            println!("Could not parse index, skipping test");
            return;
        }
    };

    if index.entries.is_empty() {
        println!("No sessions found, skipping test");
        return;
    }

    let first_session = &index.entries[0];
    let jsonl_path = &first_session.full_path;

    if !jsonl_path.exists() {
        println!("JSONL file not found at {:?}, skipping test", jsonl_path);
        return;
    }

    let result = parse_last_n_entries(jsonl_path, 10);
    assert!(result.is_ok(), "Failed to parse JSONL: {:?}", result.err());

    let entries = result.unwrap();
    println!("Successfully parsed {} entries from JSONL", entries.len());

    // Extract messages
    let messages = extract_messages(&entries);
    println!("Extracted {} messages", messages.len());

    // Verify messages have timestamps and content
    for (timestamp, msg_type, content, _images) in messages.iter().take(3) {
        assert!(!timestamp.is_empty(), "Timestamp should not be empty");
        assert!(!content.is_empty(), "Content should not be empty");
        println!(
            "Message type: {:?}, content length: {}",
            msg_type,
            content.len()
        );
    }
}

#[test]
fn test_message_type_extraction() {
    let home = match env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };

    let index_path = PathBuf::from(&home)
        .join(".claude/projects/-Users-vincent-m-lee--claude/sessions-index.json");

    if !index_path.exists() {
        return;
    }

    let index = match parse_sessions_index(&index_path) {
        Ok(i) => i,
        Err(_) => return,
    };

    if index.entries.is_empty() {
        return;
    }

    for entry in index.entries.iter().take(2) {
        if !entry.full_path.exists() {
            continue;
        }

        match parse_last_n_entries(&entry.full_path, 20) {
            Ok(entries) => {
                let messages = extract_messages(&entries);

                // Count message types
                let user_count = messages
                    .iter()
                    .filter(|(_, t, _, _)| t == &MessageType::User)
                    .count();
                let assistant_count = messages
                    .iter()
                    .filter(|(_, t, _, _)| t == &MessageType::Assistant)
                    .count();

                println!(
                    "Session {}: {} user messages, {} assistant messages",
                    entry.session_id, user_count, assistant_count
                );

                // There should be at least some messages
                assert!(
                    messages.len() > 0,
                    "Should have extracted at least one message"
                );
            }
            Err(e) => {
                println!("Could not parse session {}: {}", entry.session_id, e);
            }
        }
    }
}
