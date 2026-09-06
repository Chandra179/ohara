//! `robots.txt` (§8 Stage 1 politeness): a minimal RFC 9309 subset — user-agent
//! groups, `Allow`/`Disallow` with `*` and `$` wildcards, longest-match wins.
//! Pure and total: malformed input degrades to "no rules" rather than failing.

use std::collections::HashMap;

/// One parsed rule from a user-agent group.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    /// `true` for `Allow`, `false` for `Disallow`.
    allow: bool,
    /// The path pattern (`*` = any run, `$` = end anchor).
    pattern: String,
}

/// A parsed `robots.txt`. Group selection: the rules of every group whose
/// user-agent token matches the crawler's (case-insensitive substring token per
/// RFC 9309), else the `*` group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Robots {
    /// user-agent token (lowercased) → its group's rules.
    groups: HashMap<String, Vec<Rule>>,
}

impl Robots {
    /// Parses `robots.txt` text. Total: syntax errors and unknown lines are
    /// skipped, per the RFC's be-liberal parsing guidance.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        // RFC 9309: the *last* (most specific) matching group wins; groups are
        // keyed by their user-agent tokens and kept in file order.
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Vec<Rule>> = HashMap::new();
        let mut current_agents: Vec<String> = Vec::new();

        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    let agent = value.to_ascii_lowercase();
                    if !current_agents.iter().any(|a| a == &agent) {
                        // A user-agent line starts a new group unless it continues
                        // the previous group's agent list.
                        if order.last().is_none_or(|last| *last != agent) {
                            current_agents.clear();
                        }
                        current_agents.push(agent.clone());
                        if !groups.contains_key(&agent) {
                            order.push(agent.clone());
                        }
                        groups.entry(agent).or_default();
                    }
                }
                "allow" | "disallow" => {
                    let rule = Rule {
                        allow: key == "allow",
                        pattern: value.to_string(),
                    };
                    for agent in &current_agents {
                        groups.entry(agent.clone()).or_default().push(rule.clone());
                    }
                }
                _ => {} // crawl-delay, sitemap, unknown: ignored (politeness floor is ours)
            }
        }

        Self { groups }
    }

    /// Whether `path` (the URL path + query, as sent) may be fetched by `agent`
    /// (e.g. `"ohara"`). RFC 9309 longest-match: the most specific rule by
    /// pattern length wins; a tie goes to `Allow`.
    #[must_use]
    pub fn allows(&self, agent: &str, path: &str) -> bool {
        let agent = agent.to_ascii_lowercase();
        let rules = self.rules_for(&agent);
        let mut best: Option<(usize, bool)> = None; // (pattern length, allow)
        for rule in rules {
            if !matches_pattern(&rule.pattern, path) {
                continue;
            }
            let len = rule
                .pattern
                .strip_suffix('$')
                .unwrap_or(&rule.pattern)
                .len();
            let better = match best {
                None => true,
                Some((best_len, best_allow)) => {
                    len > best_len || (len == best_len && rule.allow && !best_allow)
                }
            };
            if better {
                best = Some((len, rule.allow));
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }

    /// The rules of the most specific group matching `agent`, or the `*` group.
    fn rules_for(&self, agent: &str) -> &[Rule] {
        let mut specific: Option<&Vec<Rule>> = None;
        let mut best_len = 0;
        for (token, rules) in &self.groups {
            if token == "*" {
                continue;
            }
            // RFC 9309: the agent matches when the group's token is a substring
            // of the product token ("ohara" matches "ohara/0.1").
            if agent.contains(token.as_str()) && token.len() > best_len {
                best_len = token.len();
                specific = Some(rules);
            }
        }
        specific
            .or_else(|| self.groups.get("*"))
            .map_or(&[], Vec::as_slice)
    }
}

/// Whether `pattern` matches `path` — a *prefix* of the path unless the pattern
/// ends with `$`, which anchors it to the full path. `*` matches any run.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let anchored = pattern.strip_suffix('$').unwrap_or(pattern);
    let anchored_bytes = anchored.as_bytes();
    let path_bytes = path.as_bytes();
    let mut p = 0; // index into pattern
    let mut s = 0; // index into path
    let mut star_p: Option<usize> = None;
    let mut star_s = 0;

    while p < anchored_bytes.len() {
        if anchored_bytes[p] == b'*' {
            star_p = Some(p);
            star_s = s;
            p += 1;
        } else if s < path_bytes.len() && anchored_bytes[p] == path_bytes[s] {
            p += 1;
            s += 1;
        } else if let Some(sp) = star_p {
            // Retry the run after the `*` one byte further into the path; when
            // the run is exhausted the `*` simply matched the remainder.
            if star_s >= path_bytes.len() {
                return false;
            }
            p = sp + 1;
            star_s += 1;
            s = star_s;
        } else {
            return false;
        }
    }
    if pattern.ends_with('$') {
        s == path_bytes.len()
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use super::*;

    const SAMPLE: &str = "
# comment line
User-agent: *
Disallow: /admin/
Allow: /admin/public/

User-agent: ohara
Disallow: /private/*
Disallow: *.pdf$
Crawl-delay: 5
";

    #[test]
    fn wildcard_group_rules_apply() {
        let robots = Robots::parse(SAMPLE);
        assert!(robots.allows("anyone", "/posts/1"));
        assert!(!robots.allows("anyone", "/admin/secret"));
        assert!(
            robots.allows("anyone", "/admin/public/page"),
            "Allow wins on tie/longer"
        );
    }

    #[test]
    fn specific_group_overrides_the_wildcard_group() {
        let robots = Robots::parse(SAMPLE);
        assert!(!robots.allows(
            "ohara/0.1 (+https://github.com/Chandra179/ohara)",
            "/private/x"
        ));
        assert!(
            !robots.allows("ohara", "/doc/report.pdf"),
            "$ anchors the end"
        );
        assert!(robots.allows("ohara", "/doc/report.pdfx"));
        // The wildcard group's rules do not apply to the matched specific agent.
        assert!(robots.allows("ohara", "/admin/secret"));
    }

    #[test]
    fn missing_robots_means_everything_is_allowed() {
        let robots = Robots::parse("");
        assert!(robots.allows("ohara", "/anything/at/all"));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let robots = Robots::parse("garbage\n\nDisallow\nUser-agent: ohara\nDisallow: /x\n");
        assert!(!robots.allows("ohara", "/x"));
        assert!(robots.allows("ohara", "/y"));
    }

    #[test]
    fn star_matches_runs_of_any_length() {
        let robots = Robots::parse("User-agent: *\nDisallow: /a*b\n");
        assert!(!robots.allows("ohara", "/axxxb"));
        assert!(!robots.allows("ohara", "/ab"));
        assert!(robots.allows("ohara", "/ac"));
    }
}
