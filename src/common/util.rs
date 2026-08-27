use tracing::{error, trace};

pub fn parse_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current_part = String::new();
    let mut quote_char = None;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' if quote_char.is_none() || quote_char == Some(ch) => {
                quote_char = if quote_char.is_none() { Some(ch) } else { None };
            }
            ' ' | '\t' if quote_char.is_none() => {
                if !current_part.is_empty() {
                    parts.push(current_part.clone());
                    current_part.clear();
                }
            }
            '\\' if quote_char.is_some() => {
                if let Some(next_ch) = chars.next() {
                    match next_ch {
                        'n' => current_part.push('\n'),
                        't' => current_part.push('\t'),
                        'r' => current_part.push('\r'),
                        '\\' => current_part.push('\\'),
                        '\'' => current_part.push('\''),
                        '"' => current_part.push('"'),
                        _ => {
                            current_part.push('\\');
                            current_part.push(next_ch);
                        }
                    }
                } else {
                    current_part.push('\\');
                }
            }
            _ => {
                current_part.push(ch);
            }
        }
    }

    if !current_part.is_empty() {
        parts.push(current_part);
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::parse_command;

    #[test]
    fn matching_quote_delimiter_controls_argument_grouping() {
        assert_eq!(parse_command(r#"say "it's alive""#), vec![
            "say".to_string(),
            "it's alive".to_string()
        ]);
        assert_eq!(parse_command(r#"say 'she said "hello"'"#), vec![
            "say".to_string(),
            "she said \"hello\"".to_string()
        ]);
    }

    #[test]
    fn quoted_escapes_and_unquoted_whitespace_keep_existing_behavior() {
        assert_eq!(parse_command("notify  \"line\\nvalue\"\tend"), vec![
            "notify".to_string(),
            "line\nvalue".to_string(),
            "end".to_string(),
        ]);
    }
}

fn execute_command(command: &str, context: &str) {
    let parts = parse_command(command);
    if parts.is_empty() {
        error!(%context, "Empty command");
        return;
    }

    let (cmd, args) = parts.split_first().unwrap();
    let output = std::process::Command::new(cmd).args(args).output();
    match output {
        Ok(output) if output.status.success() => {
            trace!(%context, %command, "Command completed successfully");
        }
        Ok(output) => {
            error!(%context, %command, status = %output.status, "Command failed");
            if !output.stderr.is_empty() {
                error!(%context, "stderr: {}", String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(error) => {
            error!(%context, %command, ?error, "Failed to execute command");
        }
    }
}

pub fn execute_commands_sync(commands: &[String], context: &str) {
    if commands.is_empty() {
        return;
    }

    trace!(%context, count = commands.len(), "Executing commands synchronously");
    for command in commands {
        execute_command(command, context);
    }
}

pub fn execute_commands_async(commands: &[String], context: &str) {
    if commands.is_empty() {
        return;
    }

    trace!(%context, count = commands.len(), "Scheduling commands");
    let commands = commands.to_vec();
    let context = context.to_string();
    std::thread::spawn(move || {
        for command in commands {
            execute_command(&command, &context);
        }
    });
}

pub fn execute_startup_commands(commands: &[String]) {
    execute_commands_async(commands, "startup");
}
