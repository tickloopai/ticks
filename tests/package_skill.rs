//! Checks shared by every package's shipped Skill, included by each package's integration tests.

/// Every tool the Tickloop Agent can offer the model, by name. Keep in sync with tickloopai/tickloop:
/// `built_in_definitions` and `ARTIFACT_TOOL_NAMES` in `agent/runtime`, and the shared Task tools.
const AGENT_TOOLS: [&str; 37] = [
    "ArtifactComment",
    "ArtifactEdit",
    "ArtifactLogin",
    "ArtifactRead",
    "ArtifactUpload",
    "AskUserQuestion",
    "Bash",
    "Edit",
    "EnterPlanMode",
    "ExitPlanMode",
    "Glob",
    "Grep",
    "ListMcpResourcesTool",
    "ListSubagents",
    "Read",
    "ReadMcpResourceTool",
    "SendMessage",
    "Skill",
    "StartGoal",
    "StopSubagent",
    "Subagent",
    "SubagentHandback",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskStop",
    "TaskUpdate",
    "TickCall",
    "TickDescribe",
    "TickManage",
    "TickRead",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
    "Write",
    // Not tools, but terms the Agent's tools use.
    "OutcomeUnknown",
    "TickUpdate",
];

/// Tickloop replaces this with the installed package's directory when it loads a package Skill.
const PACKAGE_ROOT_VARIABLE: &str = "${TICK_PACKAGE_ROOT}";

/// A package Skill opens the installed `program` by name, leaves logging in to Tickloop, and names
/// only tools the Agent offers by their compound names, such as `TickManage`.
pub fn assert_shipped_skill(skill: &str, program: &str) {
    assert!(skill.contains(&format!("[\"{program}\", ")), "{skill}");
    assert!(!skill.contains(PACKAGE_ROOT_VARIABLE), "{skill}");
    for retired in [
        format!("bin/{program} auth"),
        "auth status".into(),
        "`! ".into(),
        "tickloop tick".into(),
        "--region".into(),
    ] {
        assert!(!skill.contains(&retired), "the Skill still says {retired}");
    }
    let named = skill
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| is_compound_name(word))
        .collect::<Vec<_>>();
    assert!(named.contains(&"TickManage"), "{skill}");
    for word in named {
        assert!(
            AGENT_TOOLS.contains(&word),
            "the Skill names {word}, which is no Agent tool"
        );
    }
}

/// Two or more capitalized ASCII segments, such as `TickManage`.
fn is_compound_name(word: &str) -> bool {
    let mut segments = 0;
    let mut characters = word.chars().peekable();
    while let Some(first) = characters.next() {
        let mut lowercase = 0;
        while characters.next_if(char::is_ascii_lowercase).is_some() {
            lowercase += 1;
        }
        if !first.is_ascii_uppercase() || lowercase == 0 {
            return false;
        }
        segments += 1;
    }
    segments > 1
}
