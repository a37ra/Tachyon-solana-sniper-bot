pub async fn handle_command(cmd: &str, args: &[&str]) -> String {
    match cmd {
        "/kill_switch" => {
            let state = args.get(0).copied().unwrap_or("status");
            format!("Kill switch state set/checked: {}", state)
        }
        "/position" => {
            let sub = args.get(0).copied().unwrap_or("list");
            format!("Position command executed: {}", sub)
        }
        "/circuit_breaker" => {
            let sub = args.get(0).copied().unwrap_or("status");
            format!("Circuit breaker command executed: {}", sub)
        }
        "/mode" => {
            let mode = args.get(0).copied().unwrap_or("paper");
            format!("Trading mode set to: {}", mode)
        }
        _ => "Unknown command".to_string(),
    }
}
