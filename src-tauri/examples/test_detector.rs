use c9watch_lib::session::SessionDetector;

fn main() {
    println!("Claude Session Detector Test\n");

    // Create a new detector
    let mut detector = match SessionDetector::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to create detector: {}", e);
            return;
        }
    };

    // Detect active sessions
    match detector.detect_sessions() {
        Ok((sessions, _diagnostics)) => {
            if sessions.is_empty() {
                println!("No active Claude Code sessions found.");
            } else {
                println!("Found {} active Claude Code session(s):\n", sessions.len());
                for (i, session) in sessions.iter().enumerate() {
                    println!("Session {}:", i + 1);
                    println!("  PID: {}", session.pid);
                    println!("  Project: {}", session.project_name);
                    println!("  Working Directory: {}", session.cwd.display());
                    println!("  Project Path: {}", session.project_path.display());
                    if let Some(session_id) = &session.session_id {
                        println!("  Session ID: {}", session_id);
                    }
                    println!();
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to detect sessions: {}", e);
        }
    }
}
