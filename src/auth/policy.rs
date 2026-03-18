use crate::auth::KeyRecord;

impl KeyRecord {
    pub fn allows_model(&self, model: &str) -> bool {
        if self
            .blacklist_models
            .iter()
            .any(|entry| model_rule_matches(entry, model))
        {
            return false;
        }
        if self.whitelist_models.is_empty() {
            return true;
        }
        self.whitelist_models
            .iter()
            .any(|entry| model_rule_matches(entry, model))
    }
}

fn model_rule_matches(rule: &str, model: &str) -> bool {
    if rule == model {
        return true;
    }

    let (rule_name, rule_tag) = split_model_tag(rule);
    let (model_name, model_tag) = split_model_tag(model);
    if rule_name != model_name {
        return false;
    }

    match (rule_tag, model_tag) {
        (None, _) => true,
        (Some("latest"), None) => true,
        _ => false,
    }
}

fn split_model_tag(value: &str) -> (&str, Option<&str>) {
    match value.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name, Some(tag)),
        _ => (value, None),
    }
}
