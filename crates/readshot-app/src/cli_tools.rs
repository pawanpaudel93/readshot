pub fn command_line_tools_instructions() -> &'static str {
    r#"mkdir -p "$HOME/bin"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot" "$HOME/bin/readshot"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot-mcp" "$HOME/bin/readshot-mcp"
grep -q 'export PATH="$HOME/bin:$PATH"' "$HOME/.zshrc" || echo 'export PATH="$HOME/bin:$PATH"' >> "$HOME/.zshrc"
source "$HOME/.zshrc"
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
        assert!(instructions.contains("readshot --help"));
    }
}
