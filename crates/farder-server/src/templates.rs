use anyhow::Result;
use farder_protocol::server::ChannelType;
use rusqlite::Connection;
use serde::Deserialize;

const BLANK: &str = include_str!("../templates/blank.toml");
const FRIEND_GROUP: &str = include_str!("../templates/friend-group.toml");
const GAMING_COMMUNITY: &str = include_str!("../templates/gaming-community.toml");
const ORGANIZATION: &str = include_str!("../templates/organization.toml");
const PUBLIC_COMMUNITY: &str = include_str!("../templates/public-community.toml");

#[derive(Debug, Deserialize)]
pub struct Template {
    pub template: TemplateInfo,
    #[serde(default)]
    pub roles: Option<Vec<TemplateRole>>,
    pub categories: Vec<TemplateCategory>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct TemplateRole {
    pub name: String,
    pub permissions: u64,
    pub color: Option<String>,
    pub position: u32,
}

#[derive(Debug, Deserialize)]
pub struct TemplateCategory {
    pub name: String,
    #[serde(default)]
    pub channels: Vec<TemplateChannel>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateChannel {
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
}

pub fn parse_template(toml_str: &str) -> Result<Template> {
    toml::from_str(toml_str).map_err(Into::into)
}

pub fn list_builtin_templates() -> Vec<Template> {
    [BLANK, FRIEND_GROUP, GAMING_COMMUNITY, ORGANIZATION, PUBLIC_COMMUNITY]
        .iter()
        .filter_map(|s| parse_template(s).ok())
        .collect()
}

pub fn load_templates_from_dir(dir: &str) -> Vec<Template> {
    let path = std::path::Path::new(dir);
    if !path.exists() || !path.is_dir() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut templates = Vec::new();
    for entry in entries.flatten() {
        let file_path = entry.path();
        if file_path.extension().and_then(|e| e.to_str()) == Some("toml") {
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                if let Ok(tmpl) = parse_template(&content) {
                    templates.push(tmpl);
                }
            }
        }
    }
    templates
}

pub fn apply_template(conn: &Connection, template: &Template) -> Result<()> {
    // Create roles first (if any).
    if let Some(roles) = &template.roles {
        for role in roles {
            crate::members::create_role(
                conn,
                &role.name,
                role.permissions,
                role.color.as_deref(),
                role.position,
                false,
                false,
            )?;
        }
    }

    // Create categories and their channels.
    for (cat_pos, category) in template.categories.iter().enumerate() {
        let cat_id = crate::channels::create_category(conn, &category.name, cat_pos as u32)?;

        for (ch_pos, channel) in category.channels.iter().enumerate() {
            let channel_type = if channel.channel_type == "announcement" {
                ChannelType::Announcement
            } else {
                ChannelType::Text
            };
            crate::channels::create_channel(
                conn,
                &channel.name,
                channel_type,
                Some(cat_id),
                ch_pos as u32,
            )?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{list_categories, list_channels};
    use crate::db;
    use crate::members::list_roles;

    #[test]
    fn test_parse_blank_template() {
        let tmpl = parse_template(BLANK).expect("blank.toml should parse");
        assert_eq!(tmpl.template.name, "Blank");
        assert_eq!(tmpl.categories.len(), 1);
        assert_eq!(tmpl.categories[0].channels.len(), 1);
        // no roles
        assert!(tmpl.roles.as_ref().map(|r| r.is_empty()).unwrap_or(true));
    }

    #[test]
    fn test_parse_gaming_template() {
        let tmpl = parse_template(GAMING_COMMUNITY).expect("gaming-community.toml should parse");
        assert_eq!(tmpl.template.name, "Gaming Community");
        let roles = tmpl.roles.as_ref().expect("should have roles");
        assert_eq!(roles.len(), 3);
        assert_eq!(tmpl.categories.len(), 3);
    }

    #[test]
    fn test_list_builtin_templates() {
        let templates = list_builtin_templates();
        assert_eq!(templates.len(), 5);
        let names: Vec<&str> = templates.iter().map(|t| t.template.name.as_str()).collect();
        assert!(names.contains(&"Blank"));
        assert!(names.contains(&"Friend Group"));
        assert!(names.contains(&"Gaming Community"));
        assert!(names.contains(&"Organization"));
        assert!(names.contains(&"Public Community"));
    }

    #[test]
    fn test_apply_blank_template() {
        let conn = db::open_in_memory().unwrap();
        let tmpl = parse_template(BLANK).unwrap();
        apply_template(&conn, &tmpl).unwrap();

        let channels = list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "general");

        let categories = list_categories(&conn).unwrap();
        assert_eq!(categories.len(), 1);
        assert_eq!(categories[0].name, "General");
    }

    #[test]
    fn test_apply_gaming_template() {
        let conn = db::open_in_memory().unwrap();
        let tmpl = parse_template(GAMING_COMMUNITY).unwrap();
        apply_template(&conn, &tmpl).unwrap();

        let roles = list_roles(&conn).unwrap();
        assert_eq!(roles.len(), 3);

        let channels = list_channels(&conn).unwrap();
        assert_eq!(channels.len(), 5);

        let categories = list_categories(&conn).unwrap();
        assert_eq!(categories.len(), 3);
    }
}
