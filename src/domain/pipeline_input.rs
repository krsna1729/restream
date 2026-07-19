use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineInputRole {
    Primary,
    Backup,
}

impl PipelineInputRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Backup => "backup",
        }
    }
}

impl TryFrom<&str> for PipelineInputRole {
    type Error = PipelineInputRoleParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "primary" => Ok(Self::Primary),
            "backup" => Ok(Self::Backup),
            _ => Err(PipelineInputRoleParseError(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInputRoleParseError(String);

impl fmt::Display for PipelineInputRoleParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown pipeline input role: {}", self.0)
    }
}

impl std::error::Error for PipelineInputRoleParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInput {
    pub id: String,
    pub pipeline_id: String,
    pub label: String,
    pub stream_key: String,
    pub role: PipelineInputRole,
    pub enabled: bool,
    pub selected: bool,
}
