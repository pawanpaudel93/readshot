#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub const ALL: [Shell; 3] = [Shell::Zsh, Shell::Bash, Shell::Fish];

    pub fn label(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
        }
    }
}

const COMMON_COMMANDS: &str = r#"mkdir -p "$HOME/.local/bin"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot" "$HOME/.local/bin/readshot"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot-mcp" "$HOME/.local/bin/readshot-mcp""#;

const ZSH_COMMANDS: &str = r#"export PATH="$HOME/.local/bin:$PATH"
grep -qxF 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/.zshrc" 2>/dev/null || echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.zshrc""#;

const BASH_COMMANDS: &str = r#"export PATH="$HOME/.local/bin:$PATH"
grep -qxF 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/.bashrc" 2>/dev/null || echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.bashrc""#;

const FISH_COMMANDS: &str = r#"fish_add_path "$HOME/.local/bin"
mkdir -p "$HOME/.config/fish"
grep -qxF 'fish_add_path "$HOME/.local/bin"' "$HOME/.config/fish/config.fish" 2>/dev/null || echo 'fish_add_path "$HOME/.local/bin"' >> "$HOME/.config/fish/config.fish""#;

const VERIFY_COMMANDS: &str = r#"readshot --help"#;

pub fn common_commands() -> &'static str {
    COMMON_COMMANDS
}

pub fn shell_commands(shell: Shell) -> &'static str {
    match shell {
        Shell::Zsh => ZSH_COMMANDS,
        Shell::Bash => BASH_COMMANDS,
        Shell::Fish => FISH_COMMANDS,
    }
}

pub fn verify_commands() -> &'static str {
    VERIFY_COMMANDS
}

pub fn setup_commands(shell: Shell) -> String {
    format!(
        "{}\n\n{}\n\n{}",
        common_commands(),
        shell_commands(shell),
        verify_commands()
    )
}

pub fn command_line_tools_instructions() -> String {
    format!(
        r#"Common symlink commands:

{}

For zsh:
{}

For bash:
{}

For fish:
{}

Verify:
{}"#,
        common_commands(),
        shell_commands(Shell::Zsh),
        shell_commands(Shell::Bash),
        shell_commands(Shell::Fish),
        verify_commands()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_create_local_bin_symlinks_and_update_path() {
        let instructions = command_line_tools_instructions();

        assert!(instructions.contains("mkdir -p \"$HOME/.local/bin\""));
        assert!(instructions.contains(
            "ln -sf \"/Applications/Readshot.app/Contents/MacOS/readshot\" \"$HOME/.local/bin/readshot\""
        ));
        assert!(instructions.contains(
            "ln -sf \"/Applications/Readshot.app/Contents/MacOS/readshot-mcp\" \"$HOME/.local/bin/readshot-mcp\""
        ));
        assert!(instructions.contains("export PATH=\"$HOME/.local/bin:$PATH\""));
        assert!(instructions.contains(".zshrc"));
        assert!(instructions.contains(".bashrc"));
        assert!(instructions.contains("fish_add_path \"$HOME/.local/bin\""));
        assert!(instructions.contains("config.fish"));
        assert!(instructions.contains("readshot --help"));
    }

    #[test]
    fn setup_commands_include_only_selected_shell() {
        let zsh = setup_commands(Shell::Zsh);
        assert!(zsh.contains(".zshrc"));
        assert!(!zsh.contains(".bashrc"));
        assert!(!zsh.contains("config.fish"));

        let bash = setup_commands(Shell::Bash);
        assert!(bash.contains(".bashrc"));
        assert!(!bash.contains(".zshrc"));
        assert!(!bash.contains("config.fish"));

        let fish = setup_commands(Shell::Fish);
        assert!(fish.contains("config.fish"));
        assert!(!fish.contains(".zshrc"));
        assert!(!fish.contains(".bashrc"));
    }
}
