//! A launch plan: the exact program, arguments, environment and working directory for a game, with secrets kept
//! out of it. The Epic exchange code is a placeholder until the very moment the process is spawned, so a plan
//! can be printed, logged and compared in tests without leaking anything.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

/// A secret string. `Debug` and `Display` never show it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    /// The only way to read it. Call this where the value is used, not where it is passed around.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanArg {
    Lit(String),
    /// `<prefix><exchange code>`, e.g. `-AUTH_PASSWORD=` + code.
    ExchangeCode { prefix: String },
}

impl PlanArg {
    pub fn lit(s: impl Into<String>) -> Self {
        PlanArg::Lit(s.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<PlanArg>,
    /// Environment *overrides* on top of the parent's environment.
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// Human-readable remarks (e.g. "offline mode"), shown by the `plan` example.
    pub notes: Vec<String>,
    /// GameMode is asked for the game's own processes once they run, through this `gamemoded`
    /// (see [`crate::gamemode`]); None = no GameMode, or the `gamemoderun` wrapper is in `program`/`args`.
    pub gamemode: Option<PathBuf>,
}

impl LaunchPlan {
    /// Real arguments, with the exchange code filled in. Only [`crate::spawn`] calls this with `Some`.
    pub fn resolve_args(&self, code: Option<&Secret>) -> Vec<String> {
        self.args
            .iter()
            .filter_map(|a| match a {
                PlanArg::Lit(s) => Some(s.clone()),
                PlanArg::ExchangeCode { prefix } => Some(format!("{prefix}{}", code.map(|c| c.expose()).unwrap_or(""))),
            })
            .collect()
    }

    /// The command line as shell-quoted text with placeholders instead of secrets. Safe to print.
    pub fn display_command(&self) -> String {
        let mut parts = vec![shell_quote(&self.program)];
        for a in &self.args {
            parts.push(match a {
                PlanArg::Lit(s) => shell_quote(s),
                PlanArg::ExchangeCode { prefix } => shell_quote(&format!("{prefix}<exchange-code>")),
            });
        }
        parts.join(" ")
    }
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_shows() {
        let s = Secret::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert!(!format!("{s}").contains("hunter2"));
    }

    #[test]
    fn display_uses_placeholder_and_quotes() {
        let plan = LaunchPlan {
            program: "umu-run".into(),
            args: vec![PlanArg::lit("/g/My Game.exe"), PlanArg::ExchangeCode { prefix: "-AUTH_PASSWORD=".into() }],
            ..Default::default()
        };
        let text = plan.display_command();
        assert_eq!(text, "umu-run '/g/My Game.exe' '-AUTH_PASSWORD=<exchange-code>'");
        assert!(plan.resolve_args(Some(&Secret::new("abc")))[1].ends_with("abc"));
    }
}
