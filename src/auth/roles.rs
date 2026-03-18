use crate::auth::Role;

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "Admin",
            Role::Client => "Client",
            Role::Worker => "Worker",
        }
    }
}
