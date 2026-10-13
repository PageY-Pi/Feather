//! Pi-compatible resource discovery and slash expansion for Feather.
//!
//! Pure data: discovers skills and prompt templates; it does not execute extensions/packages.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use serde_yaml::Value as Yaml;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const CONFIG_DIR_NAME: &str = ".feather";
const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;

const MAX_SKILL_METADATA_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub kind: String,
    pub message: String,
    pub path: Option<PathBuf>,
}
impl Diagnostic {
    fn warn(path: impl Into<Option<PathBuf>>, msg: impl Into<String>) -> Self {
        Self {
            kind: "warning".into(),
            message: msg.into(),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub content: String,
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLoad {
    pub skills: Vec<Skill>,
    pub prompts: Vec<PromptTemplate>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ResourceLoad {
    /// Expand a user's slash input using Pi ordering: explicit `/skill[: ]name` first, then prompt templates.
    pub fn expand_user_prompt(&self, text: &str) -> Result<String, String> {
        expand_user_prompt(text, self)
    }

    /// Load one skill body by name. The body is read fresh and frontmatter is stripped on every call.
    pub fn expand_skill(&self, name: &str, request: &str) -> Result<String, String> {
        let sk = self
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("unknown skill: {name}"))?;
        explicit_skill_prompt(sk, request).map_err(|e| e.to_string())
    }

    /// Public metadata-only listing for `/skill` chooser/list UIs. Never includes skill bodies.
    pub fn skill_listing(&self) -> String {
        if self.skills.is_empty() {
            return "No skills found.".into();
        }
        let mut lines = vec!["Available skills:".to_string()];
        for s in &self.skills {
            lines.push(format!("/skill {} — {}", s.name, s.description));
        }
        lines.join("\n")
    }

    /// Slash completion pairs: `(command, description)` for templates and skills.
    pub fn slash_completions(&self, enable_skill_commands: bool) -> Vec<(String, String)> {
        let mut out: Vec<_> = self
            .prompts
            .iter()
            .map(|p| (format!("/{}", p.name), p.description.clone()))
            .collect();
        if enable_skill_commands {
            out.push(("/skill".into(), "list available skills".into()));
            out.extend(
                self.skills
                    .iter()
                    .map(|s| (format!("/skill:{}", s.name), s.description.clone())),
            );
        }
        out
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResourceOptions {
    pub cwd: PathBuf,
    pub feather_dir: PathBuf,
    pub skill_paths: Vec<String>,
    pub prompt_paths: Vec<String>,
    pub include_defaults: bool,
}

pub fn load_resources(options: &ResourceOptions) -> ResourceLoad {
    let (skills, mut diagnostics) = load_skills(
        &options.cwd,
        &options.feather_dir,
        &options.skill_paths,
        options.include_defaults,
    );
    let (prompts, d) = load_prompt_templates(
        &options.cwd,
        &options.feather_dir,
        &options.prompt_paths,
        options.include_defaults,
    );
    diagnostics.extend(d);
    ResourceLoad {
        skills,
        prompts,
        diagnostics,
    }
}

#[derive(Default)]
struct ResourceRules {
    include: Vec<PathBuf>,
    exclude: Vec<PathBuf>,
    globs: Vec<String>,
}
fn split_rules(raw: &[String], cwd: &Path, base: &Path) -> ResourceRules {
    let mut r = ResourceRules::default();
    for item in raw {
        let s = item.trim();
        if s.is_empty() {
            continue;
        }
        if let Some(rest) = s.strip_prefix('!') {
            r.globs.push(format!("!{}", rest));
        } else if let Some(rest) = s.strip_prefix('+') {
            r.include.push(resolve_resource_path(rest, cwd, base));
        } else if let Some(rest) = s.strip_prefix('-') {
            r.exclude.push(resolve_resource_path(rest, cwd, base));
        } else if has_glob(s) {
            r.globs.push(s.to_string());
        } else {
            r.include.push(resolve_resource_path(s, cwd, base));
        }
    }
    r
}
fn has_glob(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains('{')
}

pub fn load_skills(
    cwd: &Path,
    feather_dir: &Path,
    explicit: &[String],
    include_defaults: bool,
) -> (Vec<Skill>, Vec<Diagnostic>) {
    let mut map = BTreeMap::<String, Skill>::new();
    let mut real = HashSet::<PathBuf>::new();
    let mut diags = Vec::new();
    let mut add = |skills: Vec<Skill>, d: Vec<Diagnostic>| {
        diags.extend(d);
        for sk in skills {
            let rp = canonical(&sk.file_path);
            if real.contains(&rp) {
                continue;
            }
            if let Some(w) = map.get(&sk.name) {
                diags.push(Diagnostic {
                    kind: "collision".into(),
                    message: format!("name \"{}\" collision", sk.name),
                    path: Some(sk.file_path.clone()),
                });
                diags.push(Diagnostic {
                    kind: "collision".into(),
                    message: format!(
                        "winner: {}; loser: {}",
                        w.file_path.display(),
                        sk.file_path.display()
                    ),
                    path: Some(sk.file_path),
                });
            } else {
                real.insert(rp);
                map.insert(sk.name.clone(), sk);
            }
        }
    };
    if include_defaults {
        let (skills, d) = load_skills_from_dir(&feather_dir.join("skills"), true);
        add(skills, d);
        let (skills, d) = load_skills_from_dir(&cwd.join(CONFIG_DIR_NAME).join("skills"), true);
        add(skills, d);
    }
    let rules = split_rules(explicit, cwd, feather_dir);
    let excluded: HashSet<_> = rules.exclude.iter().map(|p| canonical(p)).collect();
    for p in &rules.include {
        if excluded.contains(&canonical(p)) {
            continue;
        }
        load_skill_path(p, &mut add);
    }
    if !rules.globs.is_empty() {
        let (inc, exc, d) = compile_globs(&rules.globs);
        add(Vec::new(), d);
        scan_globbed_skills(feather_dir, &inc, &exc, &mut add);
        scan_globbed_skills(cwd, &inc, &exc, &mut add);
    }
    (map.into_values().collect(), diags)
}
fn load_skill_path<F: FnMut(Vec<Skill>, Vec<Diagnostic>)>(p: &Path, add: &mut F) {
    match fs::metadata(p) {
        Ok(m) if m.is_dir() => {
            let (s, d) = load_skills_from_dir(p, true);
            add(s, d);
        }
        Ok(m) if m.is_file() && p.extension().and_then(|s| s.to_str()) == Some("md") => {
            let (s, d) = load_skill_file(p);
            add(s.into_iter().collect(), d);
        }
        Ok(_) => add(
            Vec::new(),
            vec![Diagnostic::warn(
                Some(p.to_path_buf()),
                "skill path is not a markdown file",
            )],
        ),
        Err(_) => add(
            Vec::new(),
            vec![Diagnostic::warn(
                Some(p.to_path_buf()),
                "skill path does not exist",
            )],
        ),
    }
}

pub fn load_skills_from_dir(dir: &Path, include_root_files: bool) -> (Vec<Skill>, Vec<Diagnostic>) {
    let mut seen = HashSet::new();
    load_skills_dir_inner(dir, include_root_files, &mut seen, 0)
}
fn load_skills_dir_inner(
    dir: &Path,
    include_root_files: bool,
    seen: &mut HashSet<PathBuf>,
    depth: usize,
) -> (Vec<Skill>, Vec<Diagnostic>) {
    let mut skills = Vec::new();
    let mut diags = Vec::new();
    if depth > 64 {
        diags.push(Diagnostic::warn(
            Some(dir.to_path_buf()),
            "skill scan depth exceeds 64 directories",
        ));
        return (skills, diags);
    }
    if !dir.exists() {
        return (skills, diags);
    }
    let c = canonical(dir);
    if !seen.insert(c) {
        diags.push(Diagnostic::warn(
            Some(dir.to_path_buf()),
            "symlink cycle while scanning skills",
        ));
        return (skills, diags);
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return (skills, diags),
    };
    let skill_md = dir.join("SKILL.md");
    if skill_md.is_file() {
        let (s, d) = load_skill_file(&skill_md);
        if let Some(s) = s {
            skills.push(s);
        }
        diags.extend(d);
        return (skills, diags);
    }
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let md = match fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            let (s, d) = load_skills_dir_inner(&p, false, seen, depth + 1);
            skills.extend(s);
            diags.extend(d);
        } else if include_root_files
            && md.is_file()
            && p.extension().and_then(|s| s.to_str()) == Some("md")
        {
            let (s, d) = load_skill_file(&p);
            if let Some(s) = s {
                skills.push(s);
            }
            diags.extend(d);
        }
    }
    (skills, diags)
}

fn load_skill_file(path: &Path) -> (Option<Skill>, Vec<Diagnostic>) {
    let mut d = Vec::new();
    let declared = path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md");
    let fm = match read_skill_frontmatter(path) {
        Ok(fm) => fm,
        Err(e) => {
            if declared || e.kind != SkillMetadataErrorKind::NoFrontmatter {
                d.push(Diagnostic::warn(Some(path.to_path_buf()), e.message));
            }
            return (None, d);
        }
    };
    let desc = yaml_string(&fm, "description").unwrap_or_default();
    if desc.trim().is_empty() {
        if declared {
            d.push(Diagnostic::warn(
                Some(path.to_path_buf()),
                "description is required",
            ));
        }
        return (None, d);
    }
    if desc.len() > MAX_DESCRIPTION_LENGTH {
        d.push(Diagnostic::warn(
            Some(path.to_path_buf()),
            format!(
                "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
                desc.len()
            ),
        ));
    }
    let base = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let name = yaml_string(&fm, "name").unwrap_or_else(|| {
        base.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });
    for e in validate_name(&name) {
        d.push(Diagnostic::warn(Some(path.to_path_buf()), e));
    }
    (
        Some(Skill {
            name,
            description: desc,
            file_path: path.to_path_buf(),
            base_dir: base,
            disable_model_invocation: yaml_bool(&fm, "disable-model-invocation"),
        }),
        d,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillMetadataErrorKind {
    Io,
    NoFrontmatter,
    Unterminated,
    TooLarge,
    Yaml,
}

struct SkillMetadataError {
    kind: SkillMetadataErrorKind,
    message: String,
}

fn skill_metadata_error(
    kind: SkillMetadataErrorKind,
    message: impl Into<String>,
) -> SkillMetadataError {
    SkillMetadataError {
        kind,
        message: message.into(),
    }
}

fn read_skill_frontmatter(path: &Path) -> Result<Yaml, SkillMetadataError> {
    let file = fs::File::open(path)
        .map_err(|e| skill_metadata_error(SkillMetadataErrorKind::Io, e.to_string()))?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    let mut bytes = reader
        .read_line(&mut first)
        .map_err(|e| skill_metadata_error(SkillMetadataErrorKind::Io, e.to_string()))?;
    if bytes == 0 {
        return Err(skill_metadata_error(
            SkillMetadataErrorKind::NoFrontmatter,
            "description is required",
        ));
    }
    let first_no_bom = first.strip_prefix('\u{feff}').unwrap_or(&first);
    if !first_no_bom.starts_with("---") {
        return Err(skill_metadata_error(
            SkillMetadataErrorKind::NoFrontmatter,
            "description is required",
        ));
    }
    let mut yaml = String::new();
    if first_no_bom.trim_end_matches(|c| c == '\r' || c == '\n') != "---" {
        yaml.push_str(first_no_bom.trim_start_matches("---"));
    }
    loop {
        if bytes > MAX_SKILL_METADATA_BYTES {
            return Err(skill_metadata_error(
                SkillMetadataErrorKind::TooLarge,
                format!("frontmatter exceeds {MAX_SKILL_METADATA_BYTES} bytes"),
            ));
        }
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| skill_metadata_error(SkillMetadataErrorKind::Io, e.to_string()))?;
        if n == 0 {
            return Err(skill_metadata_error(
                SkillMetadataErrorKind::Unterminated,
                "unterminated frontmatter",
            ));
        }
        bytes += n;
        if line.starts_with("---") {
            break;
        }
        yaml.push_str(&line);
    }
    serde_yaml::from_str(&yaml)
        .map_err(|e| skill_metadata_error(SkillMetadataErrorKind::Yaml, e.to_string()))
}
fn validate_name(n: &str) -> Vec<String> {
    let mut e = Vec::new();
    if n.len() > MAX_NAME_LENGTH {
        e.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            n.len()
        ))
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        e.push("name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)".into())
    }
    if n.starts_with('-') || n.ends_with('-') {
        e.push("name must not start or end with a hyphen".into())
    }
    if n.contains("--") {
        e.push("name must not contain consecutive hyphens".into())
    }
    e
}

pub fn format_skills_for_system_prompt(skills: &[Skill], read_tool: &str) -> String {
    let vis: Vec<_> = skills
        .iter()
        .filter(|s| !s.disable_model_invocation)
        .collect();
    if vis.is_empty() {
        return String::new();
    }
    let mut lines=vec!["\n\nThe following skills provide specialized instructions for specific tasks.".to_string(), match read_tool {"read"=>"Use the read tool to load a skill's file when the task matches its description.","bash"=>"Use bash to load a skill's file when the task matches its description.",_=>"Load a skill's file when the task matches its description."}.to_string(), "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".into(),"".into(),"<available_skills>".into()];
    for s in vis {
        lines.push("  <skill>".into());
        lines.push(format!("    <name>{}</name>", xml(&s.name)));
        lines.push(format!(
            "    <description>{}</description>",
            xml(&s.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            xml(&s.file_path.display().to_string())
        ));
        lines.push("  </skill>".into());
    }
    lines.push("</available_skills>".into());
    lines.join("\n")
}
pub fn explicit_skill_prompt(skill: &Skill, request: &str) -> std::io::Result<String> {
    let raw = fs::read_to_string(&skill.file_path)?;
    let body = strip_frontmatter_pi(&raw).map_err(std::io::Error::other)?;
    Ok(format!(
        "<skill name=\"{}\" base_dir=\"{}\">\n{}\n</skill>{}",
        xml(&skill.name),
        xml(&skill.base_dir.display().to_string()),
        body,
        if request.trim().is_empty() {
            String::new()
        } else {
            format!("\n\nUser request: {}", request)
        }
    ))
}
fn strip_frontmatter_pi(raw: &str) -> Result<String, String> {
    let normalized = raw
        .strip_prefix('\u{feff}')
        .unwrap_or(raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok(normalized);
    }
    let Some(rel_end) = normalized[3..].find("\n---") else {
        return Ok(normalized);
    };
    let end = rel_end + 3;
    let yaml = if end >= 4 { &normalized[4..end] } else { "" };
    if !yaml.is_empty() {
        let _: Yaml = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
    }
    Ok(normalized[end + 4..].trim().to_string())
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn load_prompt_templates(
    cwd: &Path,
    feather_dir: &Path,
    explicit: &[String],
    include_defaults: bool,
) -> (Vec<PromptTemplate>, Vec<Diagnostic>) {
    let mut list = Vec::new();
    let mut d = Vec::new();
    if include_defaults {
        let (a, b) = load_templates_from_dir(&feather_dir.join("prompts"));
        list.extend(a);
        d.extend(b);
        let (a, b) = load_templates_from_dir(&cwd.join(CONFIG_DIR_NAME).join("prompts"));
        list.extend(a);
        d.extend(b);
    }
    let rules = split_rules(explicit, cwd, feather_dir);
    let excluded: HashSet<_> = rules.exclude.iter().map(|p| canonical(p)).collect();
    for p in &rules.include {
        if excluded.contains(&canonical(p)) {
            continue;
        }
        match fs::metadata(p) {
            Ok(m) if m.is_dir() => {
                let (a, b) = load_templates_from_dir(p);
                list.extend(a);
                d.extend(b)
            }
            Ok(m) if m.is_file() && p.extension().and_then(|s| s.to_str()) == Some("md") => {
                let (a, b) = load_template_file(p);
                if let Some(a) = a {
                    list.push(a)
                };
                d.extend(b)
            }
            _ => (),
        }
    }
    if !rules.globs.is_empty() {
        let (inc, exc, gd) = compile_globs(&rules.globs);
        d.extend(gd);
        scan_globbed_prompts(feather_dir, &inc, &exc, &mut list, &mut d);
        scan_globbed_prompts(cwd, &inc, &exc, &mut list, &mut d);
    }
    let mut seen = HashSet::new();
    list.retain(|p| seen.insert(canonical(&p.file_path)));
    (list, d)
}
pub fn load_templates_from_dir(dir: &Path) -> (Vec<PromptTemplate>, Vec<Diagnostic>) {
    let mut v = Vec::new();
    let mut d = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (v, d),
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        if fs::metadata(&p).map(|m| m.is_file()).unwrap_or(false)
            && p.extension().and_then(|s| s.to_str()) == Some("md")
        {
            let (a, b) = load_template_file(&p);
            if let Some(a) = a {
                v.push(a)
            };
            d.extend(b)
        }
    }
    (v, d)
}
fn load_template_file(path: &Path) -> (Option<PromptTemplate>, Vec<Diagnostic>) {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return (
                None,
                vec![Diagnostic::warn(Some(path.to_path_buf()), e.to_string())],
            )
        }
    };
    let (fm, body) = match parse_frontmatter(&raw) {
        Ok(x) => x,
        Err(e) => return (None, vec![Diagnostic::warn(Some(path.to_path_buf()), e)]),
    };
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let desc = yaml_string(&fm, "description").unwrap_or_else(|| {
        let line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let mut s: String = line.chars().take(60).collect();
        if line.chars().count() > 60 {
            s.push_str("...")
        }
        s
    });
    Some(PromptTemplate {
        name,
        description: desc,
        argument_hint: yaml_string(&fm, "argument-hint"),
        content: body,
        file_path: path.to_path_buf(),
        base_dir: path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    })
    .map(|t| (Some(t), vec![]))
    .unwrap()
}

enum SkillInvocation<'a> {
    List,
    Load { name: &'a str, request: &'a str },
}

fn parse_skill_invocation(text: &str) -> Option<SkillInvocation<'_>> {
    if let Some(rest) = text.strip_prefix("/skill:") {
        let mut it = rest.splitn(2, char::is_whitespace);
        let name = it.next().unwrap_or("");
        let request = it.next().unwrap_or("");
        return Some(SkillInvocation::Load { name, request });
    }
    let rest = text.strip_prefix("/skill")?;
    if rest.is_empty() {
        return Some(SkillInvocation::List);
    }
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start_matches(char::is_whitespace);
    if rest.is_empty() {
        return Some(SkillInvocation::List);
    }
    let mut it = rest.splitn(2, char::is_whitespace);
    let name = it.next().unwrap_or("");
    let request = it.next().unwrap_or("");
    Some(SkillInvocation::Load { name, request })
}

pub fn expand_user_prompt(text: &str, resources: &ResourceLoad) -> Result<String, String> {
    match parse_skill_invocation(text) {
        Some(SkillInvocation::List) => Ok(resources.skill_listing()),
        Some(SkillInvocation::Load { name, request }) => resources.expand_skill(name, request),
        None => Ok(expand_prompt_template(text, &resources.prompts)),
    }
}
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    if !text.starts_with('/') {
        return text.into();
    }
    let mut it = text[1..].splitn(2, char::is_whitespace);
    let name = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("");
    if let Some(t) = templates.iter().find(|t| t.name == name) {
        substitute_args(&t.content, &parse_command_args(rest))
    } else {
        text.into()
    }
}
/// Pi's installed parser is quote-aware but not backslash-escaping; empty quoted args are ignored.
pub fn parse_command_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    for ch in s.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None
            } else {
                cur.push(ch)
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch)
        } else if ch.is_whitespace() {
            if !cur.is_empty() {
                args.push(std::mem::take(&mut cur))
            }
        } else {
            cur.push(ch)
        }
    }
    if !cur.is_empty() {
        args.push(cur)
    }
    args
}
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < content.len() {
        let b = content.as_bytes();
        if b[i] == b'$' {
            if content[i..].starts_with("$ARGUMENTS") {
                out.push_str(&args.join(" "));
                i += "$ARGUMENTS".len();
                continue;
            }
            if content[i..].starts_with("$@") {
                out.push_str(&args.join(" "));
                i += 2;
                continue;
            }
            if i + 1 < content.len() && b[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < content.len() && content.as_bytes()[j].is_ascii_digit() {
                    j += 1
                }
                let n: usize = content[i + 1..j].parse().unwrap_or(0);
                if n > 0 {
                    out.push_str(args.get(n - 1).map(String::as_str).unwrap_or(""));
                }
                i = j;
                continue;
            }
            if content[i..].starts_with("${") {
                if let Some(end) = content[i + 2..].find('}') {
                    let expr = &content[i + 2..i + 2 + end];
                    out.push_str(&expand_braced(expr, args));
                    i += end + 3;
                    continue;
                }
            }
        }
        let ch = content[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
fn expand_braced(expr: &str, args: &[String]) -> String {
    let all = args.join(" ");
    if let Some((lhs, def)) = expr.split_once(":-") {
        let val = if lhs == "@" || lhs == "ARGUMENTS" {
            &all
        } else {
            let n = lhs.parse::<usize>().unwrap_or(0);
            if n == 0 {
                ""
            } else {
                args.get(n - 1).map(String::as_str).unwrap_or("")
            }
        };
        return if val.is_empty() {
            def.into()
        } else {
            val.into()
        };
    }
    if let Some(rest) = expr.strip_prefix("@:") {
        let parts: Vec<_> = rest.split(':').collect();
        let start = parts
            .first()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let len = parts.get(1).and_then(|s| s.parse::<usize>().ok());
        let end = len.map(|l| start + l).unwrap_or(args.len());
        return args[start.min(args.len())..end.min(args.len())].join(" ");
    }
    String::new()
}

fn parse_frontmatter(raw: &str) -> Result<(Yaml, String), String> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if !raw.starts_with("---\n") && !raw.starts_with("---\r\n") {
        return Ok((Yaml::Mapping(Default::default()), raw.to_string()));
    }
    let nl = if raw.starts_with("---\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let rest = &raw[3 + nl.len()..];
    let marker = format!("{nl}---");
    let idx = rest
        .find(&marker)
        .ok_or_else(|| "unterminated frontmatter".to_string())?;
    let fm = &rest[..idx];
    let body_start = idx + marker.len();
    let body = rest[body_start..]
        .strip_prefix(nl)
        .unwrap_or(&rest[body_start..])
        .to_string();
    let y: Yaml = serde_yaml::from_str(fm).map_err(|e| e.to_string())?;
    Ok((y, body))
}
fn yaml_get<'a>(y: &'a Yaml, k: &str) -> Option<&'a Yaml> {
    y.as_mapping()?.get(Yaml::String(k.into()))
}
fn yaml_string(y: &Yaml, k: &str) -> Option<String> {
    match yaml_get(y, k)? {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
fn yaml_bool(y: &Yaml, k: &str) -> bool {
    match yaml_get(y, k) {
        Some(Yaml::Bool(b)) => *b,
        Some(Yaml::String(s)) => s == "true",
        _ => false,
    }
}
fn resolve_resource_path(raw: &str, cwd: &Path, base: &Path) -> PathBuf {
    let s = raw.trim();
    let p = if let Some(r) = s.strip_prefix("~/") {
        home_dir().join(r)
    } else {
        PathBuf::from(s)
    };
    if p.is_absolute() {
        p
    } else {
        let root = if s.starts_with('.') { cwd } else { base };
        root.join(p)
    }
}
fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
fn canonical(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}
fn compile_globs(patterns: &[String]) -> (GlobSet, GlobSet, Vec<Diagnostic>) {
    let mut inc = GlobSetBuilder::new();
    let mut exc = GlobSetBuilder::new();
    let mut has_inc = false;
    let mut d = Vec::new();
    for p in patterns {
        let (ex, b) = if let Some(x) = p.strip_prefix('!') {
            (true, x)
        } else {
            (false, p.as_str())
        };
        match Glob::new(b) {
            Ok(g) => {
                if ex {
                    exc.add(g);
                } else {
                    inc.add(g);
                    has_inc = true;
                }
            }
            Err(e) => d.push(Diagnostic::warn(None, format!("invalid glob {p}: {e}"))),
        }
    }
    if !has_inc {
        inc.add(Glob::new("**/*").unwrap());
    }
    (inc.build().unwrap(), exc.build().unwrap(), d)
}
fn scan_files(root: &Path, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if !root.exists() {
        return;
    }
    let c = canonical(root);
    if !seen.insert(c) {
        return;
    }
    let Ok(rd) = fs::read_dir(root) else { return };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with('.') || n == "node_modules" {
            continue;
        }
        if let Ok(m) = fs::metadata(&p) {
            if m.is_dir() {
                scan_files(&p, out, seen)
            } else if m.is_file() {
                out.push(p)
            }
        }
    }
}
fn scan_globbed_skills<F: FnMut(Vec<Skill>, Vec<Diagnostic>)>(
    root: &Path,
    inc: &GlobSet,
    exc: &GlobSet,
    add: &mut F,
) {
    let mut files = Vec::new();
    scan_files(root, &mut files, &mut HashSet::new());
    for p in files {
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let rel = p.strip_prefix(root).unwrap_or(&p);
        if inc.is_match(rel) && !exc.is_match(rel) {
            let (s, d) = load_skill_file(&p);
            add(s.into_iter().collect(), d)
        }
    }
}
fn scan_globbed_prompts(
    root: &Path,
    inc: &GlobSet,
    exc: &GlobSet,
    list: &mut Vec<PromptTemplate>,
    diags: &mut Vec<Diagnostic>,
) {
    let mut files = Vec::new();
    scan_files(root, &mut files, &mut HashSet::new());
    for p in files {
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let rel = p.strip_prefix(root).unwrap_or(&p);
        if inc.is_match(rel) && !exc.is_match(rel) {
            let (t, d) = load_template_file(&p);
            if let Some(t) = t {
                list.push(t)
            };
            diags.extend(d)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("feather-res-{n}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn yaml_frontmatter_multiline_and_bool() {
        let d = tmp("yaml");
        fs::create_dir_all(d.join("s")).unwrap();
        fs::write(d.join("s/SKILL.md"),"---\nname: test-skill\ndescription: >\n  one: two\n  three\ndisable-model-invocation: true\n---\nbody").unwrap();
        let (s, di) = load_skills_from_dir(&d, true);
        assert!(di.is_empty(), "{di:?}");
        assert_eq!(s[0].description, "one: two three\n");
        assert!(s[0].disable_model_invocation);
        let _ = fs::remove_dir_all(d);
    }
    #[test]
    fn prompt_args_actual_pi_edge_cases() {
        assert_eq!(parse_command_args("a '' \"\" b\\ c"), vec!["a", "b\\", "c"]);
        assert_eq!(
            substitute_args(
                "$0|$1|$ARGUMENTS|${0:-d}|${@:2:2}",
                &["a".into(), "b".into(), "c".into()]
            ),
            "|a|a b c|d|b c"
        );
    }
    #[test]
    fn skills_recurse_stop_xml_and_explicit_skill() {
        let d = tmp("skills");
        fs::create_dir_all(d.join("a/nested")).unwrap();
        fs::write(
            d.join("a/SKILL.md"),
            "---\nname: a\ndescription: A & B < C\n---\nbody",
        )
        .unwrap();
        fs::write(
            d.join("a/nested/SKILL.md"),
            "---\nname: n\ndescription: no\n---",
        )
        .unwrap();
        let (s, di) = load_skills_from_dir(&d, true);
        assert!(di.is_empty());
        assert_eq!(s.len(), 1);
        assert!(format_skills_for_system_prompt(&s, "read").contains("A &amp; B &lt; C"));
        let load = ResourceLoad {
            skills: s,
            prompts: vec![],
            diagnostics: vec![],
        };
        assert!(load
            .expand_user_prompt("/skill:a do it")
            .unwrap()
            .contains("User request: do it"));
        let _ = fs::remove_dir_all(d);
    }
    #[test]
    fn skill_listing_prompt_omits_body_but_explicit_reads_fresh() {
        let d = tmp("lazy");
        fs::create_dir_all(d.join("skills/fresh")).unwrap();
        let p = d.join("skills/fresh/SKILL.md");
        fs::write(
            &p,
            "---\nname: fresh\ndescription: Fresh skill\ndisable-model-invocation: true\n---\nold body secret",
        )
        .unwrap();
        let (s, di) = load_skills_from_dir(&d.join("skills"), true);
        assert!(di.is_empty(), "{di:?}");
        assert!(format_skills_for_system_prompt(&s, "read").is_empty());
        let load = ResourceLoad {
            skills: s,
            prompts: vec![],
            diagnostics: vec![],
        };
        assert!(!load.skill_listing().contains("old body secret"));
        fs::write(
            &p,
            "---\nname: fresh\ndescription: Fresh skill\ndisable-model-invocation: true\n---\nnew body",
        )
        .unwrap();
        let expanded = load
            .expand_user_prompt("/skill fresh literal  args")
            .unwrap();
        assert!(expanded.contains("new body"), "{expanded}");
        assert!(!expanded.contains("old body secret"), "{expanded}");
        assert!(expanded.contains("User request: literal  args"));
        let colon = load.expand_user_prompt("/skill:fresh colon args").unwrap();
        assert!(colon.contains("User request: colon args"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn skill_frontmatter_strip_unknown_and_prefix_parsing() {
        let d = tmp("skill-parse");
        fs::create_dir_all(d.join("skills/multi")).unwrap();
        fs::write(
            d.join("skills/multi/SKILL.md"),
            "---\nname: multi\ndescription: |\n  line one\n  line two\n---\n\n# Body\n\nUse me.\n",
        )
        .unwrap();
        let (s, di) = load_skills_from_dir(&d.join("skills"), true);
        assert!(di.is_empty(), "{di:?}");
        assert_eq!(s[0].description, "line one\nline two\n");
        let load = ResourceLoad {
            skills: s,
            prompts: vec![PromptTemplate {
                name: "skillful".into(),
                description: "prompt".into(),
                argument_hint: None,
                content: "prompt body".into(),
                file_path: d.join("skillful.md"),
                base_dir: d.clone(),
            }],
            diagnostics: vec![],
        };
        let expanded = load.expand_user_prompt("/skill multi").unwrap();
        assert!(expanded.contains("# Body"), "{expanded}");
        assert!(!expanded.contains("description:"), "{expanded}");
        assert_eq!(load.expand_user_prompt("/skillful").unwrap(), "prompt body");
        assert!(load
            .expand_user_prompt("/skill missing")
            .unwrap_err()
            .contains("unknown skill: missing"));
        assert!(load
            .expand_user_prompt("/skill:missing")
            .unwrap_err()
            .contains("unknown skill: missing"));
        let listing = load.expand_user_prompt("/skill").unwrap();
        assert!(listing.contains("/skill multi"));
        assert!(!listing.contains("# Body"));
        let _ = fs::remove_dir_all(d);
    }
    #[test]
    fn prompt_utf8_truncate_no_panic_and_glob_exclude() {
        let d = tmp("prompts");
        fs::create_dir_all(d.join("prompts/sub")).unwrap();
        fs::write(d.join("prompts/a.md"), format!("{}\nbody", "é".repeat(80))).unwrap();
        fs::write(d.join("prompts/sub/b.md"), "hidden").unwrap();
        let (p, di) = load_prompt_templates(
            &d,
            &d,
            &["prompts/**/*.md".into(), "!prompts/sub/**".into()],
            false,
        );
        assert!(di.is_empty());
        assert_eq!(p.len(), 1);
        assert!(p[0].description.ends_with("..."));
        let _ = fs::remove_dir_all(d);
    }
}
