/// Placeholder the sandbox sees in place of a secret's value.
pub fn placeholder(name: &str) -> String {
    format!("zapper-secret-{name}")
}

/// Names referenced as `{{secret:NAME}}` in a template.
pub fn secret_refs(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{secret:") {
        let after = &rest[start + 9..];
        match after.find("}}") {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    out
}

/// Replaces `{{secret:NAME}}` with `value(NAME)`.
pub fn render(template: &str, mut value: impl FnMut(&str) -> String) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{secret:") {
        let after = &rest[start + 9..];
        let Some(end) = after.find("}}") else { break };
        out.push_str(&rest[..start]);
        out.push_str(&value(&after[..end]));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_placeholders() {
        let t = "Bearer {{secret:gh}} and {{secret:x}}";
        assert_eq!(secret_refs(t), ["gh", "x"]);
        assert_eq!(render(t, placeholder), "Bearer zapper-secret-gh and zapper-secret-x");
        assert_eq!(render("plain {{secret:", placeholder), "plain {{secret:");
    }
}
