use std::env;
use std::path::PathBuf;

// Import from the library
use c9watch_core::session::{extract_messages, parse_last_n_entries, parse_sessions_index};

fn main() {
    let home = env::var("HOME").expect("HOME env var not set");

    // Test parsing sessions-index.json
    let index_path = PathBuf::from(&home)
        .join(".claude/projects/-Users-vincent-m-lee--claude/sessions-index.json");

    println!("Parsing sessions-index.json from: {:?}", index_path);

    match parse_sessions_index(&index_path) {
        Ok(index) => {
            println!("Successfully parsed sessions-index.json");
            println!("Version: {}", index.version);
            println!("Total sessions: {}", index.entries.len());

            if let Some(first_session) = index.entries.first() {
                println!("\nFirst session:");
                println!("  ID: {}", first_session.session_id);
                println!("  Project: {:?}", first_session.project_path);
                println!("  First prompt: {}", first_session.first_prompt);
                println!("  Message count: {}", first_session.message_count);
                println!("  Git branch: {}", first_session.git_branch);

                // Test parsing JSONL file
                println!("\nParsing last 10 entries from session JSONL...");
                match parse_last_n_entries(&first_session.full_path, 10) {
                    Ok(entries) => {
                        println!("Successfully parsed {} entries", entries.len());

                        // Extract messages
                        let messages = extract_messages(&entries);
                        println!("\nExtracted {} messages:", messages.len());

                        for (timestamp, msg_type, content, _images) in messages.iter().take(5) {
                            println!("\n[{:?}] at {}", msg_type, timestamp);
                            let preview = if content.len() > 100 {
                                format!("{}...", &content[..100])
                            } else {
                                content.clone()
                            };
                            println!("{}", preview);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error parsing JSONL: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Error parsing sessions-index.json: {}", e);
        }
    }
}
