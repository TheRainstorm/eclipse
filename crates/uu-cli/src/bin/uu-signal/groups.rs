//! Local administration of private device groups. The server never exposes
//! this management interface through its public WebSocket listener.
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};
use uu_core::internet::{
    enrollment::{group_url, Enrollment},
    server::groups::{Group, Groups},
};

#[derive(Args)]
pub struct Invitation {
    /// Public WSS endpoint, including a nonstandard port when needed.
    #[arg(long)]
    url: String,
    /// CA certificate for an IP or private-certificate endpoint.
    #[arg(long)]
    ca_file: Option<PathBuf>,
}
#[derive(Subcommand)]
pub enum Command {
    /// Create an isolated device group and print its invitation.
    Create {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        name: String,
        #[command(flatten)]
        invitation: Invitation,
    },
    /// List group identifiers and names, without credentials.
    List {
        #[arg(long)]
        file: PathBuf,
    },
    /// Print the invitation for an existing group.
    Invite {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        id: uuid::Uuid,
        #[command(flatten)]
        invitation: Invitation,
    },
}
fn invitation(group: &Group, options: &Invitation) -> Result<Enrollment> {
    let ca = options
        .ca_file
        .as_ref()
        .map(|path| {
            anyhow::ensure!(
                std::fs::metadata(path)?.len() <= 64 * 1024,
                "CA certificate is too large"
            );
            std::fs::read_to_string(path).context("cannot read the CA certificate")
        })
        .transpose()?;
    Enrollment::new(
        group.name.clone(),
        group_url(&options.url, &group.id)?,
        group.token.as_str().to_owned().into(),
        ca,
    )
}
fn create(path: &Path, name: String, options: &Invitation) -> Result<Enrollment> {
    let group = Group::new(name)?;
    let code = invitation(&group, options)?;
    // Validate the complete deliverable before publishing a new credential.
    let _ = code.encode()?;
    Groups::edit(path, |groups| {
        groups.groups.push(group);
        Ok(())
    })?;
    Ok(code)
}
fn existing(path: &Path, id: uuid::Uuid, options: &Invitation) -> Result<Enrollment> {
    let groups = Groups::load(path)?;
    let group = groups
        .groups
        .iter()
        .find(|group| group.id == id)
        .context("device group was not found")?;
    invitation(group, options)
}
pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Create {
            file,
            name,
            invitation,
        } => {
            let code = create(&file, name, &invitation)?.encode()?;
            println!("{}", code.as_str());
        }
        Command::List { file } => {
            #[derive(serde::Serialize)]
            struct Entry<'a> {
                id: String,
                name: &'a str,
            }
            let groups = Groups::load(&file)?;
            let entries: Vec<_> = groups
                .groups
                .iter()
                .map(|group| Entry {
                    id: group.id.to_string(),
                    name: &group.name,
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
        Command::Invite {
            file,
            id,
            invitation,
        } => {
            let code = existing(&file, id, &invitation)?.encode()?;
            println!("{}", code.as_str());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("eclipse-group-cli-{}", uuid::Uuid::new_v4())))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn each_created_user_gets_a_distinct_group_and_reexport_keeps_its_identity() {
        let fixture = Fixture::new();
        let file = fixture.0.join("groups.json");
        let options = Invitation {
            url: "wss://service.example:9443/v1/signal".into(),
            ca_file: None,
        };
        let alice = create(&file, "Alice".into(), &options).unwrap();
        let bob = create(&file, "Bob".into(), &options).unwrap();
        assert_ne!(alice.url, bob.url);
        assert_ne!(alice.token.as_str(), bob.token.as_str());
        let groups = Groups::load(&file).unwrap();
        assert_eq!(groups.groups.len(), 2);
        let again = existing(&file, groups.groups[0].id, &options).unwrap();
        assert_eq!(alice.url, again.url);
        assert_eq!(alice.token.as_str(), again.token.as_str());
        assert_eq!(
            Enrollment::decode(&alice.encode().unwrap()).unwrap().url,
            alice.url
        );
    }
    #[test]
    fn invalid_invitation_does_not_create_an_orphan_group() {
        let fixture = Fixture::new();
        let file = fixture.0.join("groups.json");
        let options = Invitation {
            url: "http://service.example".into(),
            ca_file: None,
        };
        assert!(create(&file, "Alice".into(), &options).is_err());
        assert!(!file.exists());
    }
}
