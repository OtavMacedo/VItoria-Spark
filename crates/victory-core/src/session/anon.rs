//! Takes personal details out of what the session files say: the user's
//! home folder becomes `<HOME>`, the title's folder `<JOGO>`, the user and
//! computer names `<USUARIO>` and `<PC>`, e-mail addresses `<EMAIL>`.
//!
//! Paths are matched without regard to ASCII case (Windows paths are not
//! case-sensitive) and in each of the spellings a log has them: with
//! backslashes, with forward slashes, and with doubled backslashes (a path
//! printed with `{:?}`).

use std::borrow::Cow;

pub struct Anonymizer {
    /// What to look for (ASCII lowercase) and what it becomes, longest
    /// first so that a folder inside another is replaced whole.
    needles: Vec<(String, &'static str)>,
}

impl Anonymizer {
    /// `home`, `game`: folders; `user`, `computer`: names.
    pub fn new(
        home: Option<&str>,
        game: Option<&str>,
        user: Option<&str>,
        computer: Option<&str>,
    ) -> Self {
        let mut needles = Vec::new();
        let mut path = |p: Option<&str>, to: &'static str| {
            let Some(p) = p else { return };
            let p = p.trim_end_matches(['\\', '/']);
            // A drive's root or less would take every path with it.
            if p.len() < 4 {
                return;
            }
            let back = p.replace('/', "\\");
            for v in [
                back.clone(),
                back.replace('\\', "/"),
                back.replace('\\', "\\\\"),
            ] {
                needles.push((v.to_ascii_lowercase(), to));
            }
        };
        path(game, "<JOGO>");
        path(home, "<HOME>");
        for (name, to) in [(user, "<USUARIO>"), (computer, "<PC>")] {
            if let Some(name) = name.filter(|n| n.len() >= 3) {
                needles.push((name.to_ascii_lowercase(), to));
            }
        }
        // Paths (longest first) before the bare names they contain.
        needles.sort_by(|a, b| {
            let path = |s: &str| s.contains(['\\', '/']);
            path(&b.0).cmp(&path(&a.0)).then(b.0.len().cmp(&a.0.len()))
        });
        needles.dedup();
        Anonymizer { needles }
    }

    /// From this process's environment, with the title's folder if known.
    pub fn from_env(game: Option<&std::path::Path>) -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let home = var("USERPROFILE").or_else(|| var("HOME"));
        let user = var("USERNAME").or_else(|| var("USER"));
        let game = game.map(|g| g.to_string_lossy().into_owned());
        Anonymizer::new(
            home.as_deref(),
            game.as_deref(),
            user.as_deref(),
            var("COMPUTERNAME").as_deref(),
        )
    }

    pub fn apply<'a>(&self, s: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(s);
        for (needle, to) in &self.needles {
            if let Some(replaced) = replace_ignoring_case(&out, needle, to) {
                out = Cow::Owned(replaced);
            }
        }
        match scrub_emails(&out) {
            Some(replaced) => Cow::Owned(replaced),
            None => out,
        }
    }
}

/// `s` with each `needle` (ASCII lowercase) replaced, or `None` if there
/// is none. Lowercasing ASCII keeps byte offsets, so they hold for `s`.
fn replace_ignoring_case(s: &str, needle: &str, to: &str) -> Option<String> {
    if needle.is_empty() || s.len() < needle.len() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    let mut found = lower.match_indices(needle).peekable();
    found.peek()?;
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for (i, _) in found {
        if i < at {
            continue;
        }
        out.push_str(&s[at..i]);
        out.push_str(to);
        at = i + needle.len();
    }
    out.push_str(&s[at..]);
    Some(out)
}

/// `s` with what looks like an e-mail address replaced, or `None`.
fn scrub_emails(s: &str) -> Option<String> {
    if !s.contains('@') {
        return None;
    }
    let b = s.as_bytes();
    let local = |c: u8| c.is_ascii_alphanumeric() || b"._%+-".contains(&c);
    let domain = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'-';
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    let mut changed = false;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'@' && i >= at {
            let mut start = i;
            while start > at && local(b[start - 1]) {
                start -= 1;
            }
            let mut end = i + 1;
            while end < b.len() && domain(b[end]) {
                end += 1;
            }
            let host = s[i + 1..end].trim_end_matches('.');
            let end = i + 1 + host.len();
            let is_email = start < i
                && host.contains('.')
                && !host.starts_with('.')
                && host.rsplit('.').next().is_some_and(|tld| {
                    tld.len() >= 2 && tld.bytes().all(|c| c.is_ascii_alphabetic())
                });
            if is_email {
                out.push_str(&s[at..start]);
                out.push_str("<EMAIL>");
                at = end;
                i = end;
                changed = true;
                continue;
            }
        }
        i += 1;
    }
    changed.then(|| {
        out.push_str(&s[at..]);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anon() -> Anonymizer {
        Anonymizer::new(
            Some(r"C:\Users\Maria"),
            Some(r"D:\Jogos\Titulo"),
            Some("Maria"),
            Some("DESKTOP-ABC123"),
        )
    }

    #[test]
    fn home_and_title_folders_become_placeholders() {
        let a = anon();
        assert_eq!(
            a.apply(r"cache at C:\Users\Maria\AppData\Local\victory\cache"),
            r"cache at <HOME>\AppData\Local\victory\cache"
        );
        assert_eq!(
            a.apply(r"mounted d:\jogos\titulo as G:"),
            "mounted <JOGO> as G:"
        );
        assert_eq!(
            a.apply(r#"C:/Users/maria/x and "C:\\Users\\Maria\\y""#),
            r#"<HOME>/x and "<HOME>\\y""#
        );
    }

    #[test]
    fn names_and_emails_are_taken_out() {
        let a = anon();
        assert_eq!(
            a.apply("user maria on desktop-abc123"),
            "user <USUARIO> on <PC>"
        );
        assert_eq!(
            a.apply("contact: someone.x@example.com, ok"),
            "contact: <EMAIL>, ok"
        );
        // Not addresses: no dot after the @, or no letters at the end.
        assert_eq!(
            a.apply("pipeline@0x1f and a@b.12"),
            "pipeline@0x1f and a@b.12"
        );
    }

    #[test]
    fn nothing_to_replace_borrows() {
        let a = anon();
        assert!(matches!(a.apply("90.1 fps"), Cow::Borrowed(_)));
    }

    #[test]
    fn short_or_missing_values_are_ignored() {
        let a = Anonymizer::new(Some("C:\\"), None, Some("ab"), None);
        assert_eq!(a.apply(r"C:\abc ab"), r"C:\abc ab");
    }

    #[test]
    fn a_folder_inside_home_is_replaced_whole() {
        let a = Anonymizer::new(
            Some(r"C:\Users\Ana"),
            Some(r"C:\Users\Ana\Jogo"),
            None,
            None,
        );
        assert_eq!(a.apply(r"C:\Users\Ana\Jogo\x.exe"), r"<JOGO>\x.exe");
    }
}
