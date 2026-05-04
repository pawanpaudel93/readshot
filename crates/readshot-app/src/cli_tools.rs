pub fn command_line_tools_instructions() -> &'static str {
    r#"Common symlink commands:

mkdir -p "$HOME/bin"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot" "$HOME/bin/readshot"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot-mcp" "$HOME/bin/readshot-mcp"

For zsh:
export PATH="$HOME/bin:$PATH"
grep -qxF 'export PATH="$HOME/bin:$PATH"' "$HOME/.zshrc" 2>/dev/null || echo 'export PATH="$HOME/bin:$PATH"' >> "$HOME/.zshrc"

For bash:
export PATH="$HOME/bin:$PATH"
grep -qxF 'export PATH="$HOME/bin:$PATH"' "$HOME/.bashrc" 2>/dev/null || echo 'export PATH="$HOME/bin:$PATH"' >> "$HOME/.bashrc"

For fish:
fish_add_path "$HOME/bin"
mkdir -p "$HOME/.config/fish"
grep -qxF 'fish_add_path "$HOME/bin"' "$HOME/.config/fish/config.fish" 2>/dev/null || echo 'fish_add_path "$HOME/bin"' >> "$HOME/.config/fish/config.fish"

Verify:
readshot --help"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_create_user_bin_symlinks_and_update_path() {
        let instructions = command_line_tools_instructions();

        assert!(instructions.contains("mkdir -p \"$HOME/bin\""));
        assert!(instructions.contains(
            "ln -sf \"/Applications/Readshot.app/Contents/MacOS/readshot\" \"$HOME/bin/readshot\""
        ));
        assert!(instructions.contains(
            "ln -sf \"/Applications/Readshot.app/Contents/MacOS/readshot-mcp\" \"$HOME/bin/readshot-mcp\""
        ));
        assert!(instructions.contains("export PATH=\"$HOME/bin:$PATH\""));
        assert!(instructions.contains(".zshrc"));
        assert!(instructions.contains(".bashrc"));
        assert!(instructions.contains("fish_add_path \"$HOME/bin\""));
        assert!(instructions.contains("config.fish"));
        assert!(instructions.contains("readshot --help"));
    }
}
