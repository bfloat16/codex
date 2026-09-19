use crate::context::environment_context::push_xml_escaped_text;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct EnvironmentMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform_os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path_convention: Option<PathConvention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shell_flavor: Option<String>,
}

impl EnvironmentMetadata {
    pub(super) fn new(cwd: &PathUri, platform_os: Option<&str>, shell: Option<&str>) -> Self {
        let path_convention = match platform_os {
            Some("windows") => Some(PathConvention::Windows),
            Some("linux" | "macos") => Some(PathConvention::Posix),
            Some(_) | None => cwd.infer_path_convention(),
        };
        let shell_flavor = matches!((platform_os, shell), (Some("windows"), Some("bash")))
            .then(|| "git-bash".to_string());
        Self {
            platform_os: platform_os.map(str::to_string),
            path_convention,
            shell_flavor,
        }
    }

    pub(super) fn push_xml_elements(&self, rendered: &mut String, indent: &str) {
        push_optional_element(rendered, indent, "platform_os", self.platform_os.as_deref());
        push_optional_element(
            rendered,
            indent,
            "path_convention",
            self.path_convention.map(path_convention_name),
        );
        push_optional_element(
            rendered,
            indent,
            "shell_flavor",
            self.shell_flavor.as_deref(),
        );
    }

    pub(super) fn has_same_diff_value(&self, previous: &Self) -> bool {
        self.platform_os == previous.platform_os
            && self.path_convention == previous.path_convention
            && self.shell_flavor == previous.shell_flavor
    }
}

fn path_convention_name(path_convention: PathConvention) -> &'static str {
    match path_convention {
        PathConvention::Posix => "posix",
        PathConvention::Windows => "windows",
    }
}

fn push_optional_element(rendered: &mut String, indent: &str, name: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    rendered.push_str(indent);
    rendered.push('<');
    rendered.push_str(name);
    rendered.push('>');
    push_xml_escaped_text(rendered, value);
    rendered.push_str("</");
    rendered.push_str(name);
    rendered.push_str(">\n");
}
