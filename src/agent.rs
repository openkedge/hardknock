// SPDX-License-Identifier: Apache-2.0

use crate::{
    Error, Result,
    core::{AgentIdentity, CommandSpec},
};

pub trait AgentAdapter {
    fn id(&self) -> &'static str;
    fn build_command(&self, task: &str) -> Result<CommandSpec>;
    fn identity(&self) -> AgentIdentity;
}

/// Parses quoting once, then substitutes the task into a single argv element.
pub struct GenericShellAdapter {
    program: String,
    args: Vec<String>,
}

impl GenericShellAdapter {
    pub fn new(template: &str) -> Result<Self> {
        let words = shell_words::split(template)
            .map_err(|e| Error::InvalidInput(format!("Invalid --agent-command quoting: {e}")))?;
        let (program, args) = words
            .split_first()
            .ok_or_else(|| Error::InvalidInput("--agent-command cannot be empty".into()))?;
        if program.is_empty()
            || program.contains("{task}")
            || args.iter().filter(|a| a.as_str() == "{task}").count() != 1
            || args.iter().any(|a| a.contains("{task}") && a != "{task}")
        {
            return Err(Error::InvalidInput("--agent-command must contain {task} exactly once, as a complete argument (for example: 'my-agent --prompt {task}').".into()));
        }
        Ok(Self {
            program: program.clone(),
            args: args.to_vec(),
        })
    }
}

impl AgentAdapter for GenericShellAdapter {
    fn id(&self) -> &'static str {
        "generic-shell"
    }

    fn build_command(&self, task: &str) -> Result<CommandSpec> {
        if task.contains('\0') {
            return Err(Error::InvalidInput("Task cannot contain a NUL byte".into()));
        }
        Ok(CommandSpec {
            environment: Default::default(),
            program: self.program.clone(),
            args: self
                .args
                .iter()
                .map(|a| {
                    if a == "{task}" {
                        task.to_owned()
                    } else {
                        a.clone()
                    }
                })
                .collect(),
        })
    }

    fn identity(&self) -> AgentIdentity {
        AgentIdentity {
            kind: self.id().into(),
            executable: self.program.clone(),
            version: None,
            model: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_task_remains_one_literal_argument() {
        let adapter = GenericShellAdapter::new("my-agent --prompt '{task}'").unwrap();
        let task = "hello '; touch /tmp/no; $(whoami)\n\" world";
        let command = adapter.build_command(task).unwrap();
        assert_eq!(command.args, ["--prompt", task]);
        assert_eq!(adapter.identity().kind, "generic-shell");
    }

    #[test]
    fn explicit_shell_is_supported_but_ambiguous_templates_are_rejected() {
        assert_eq!(
            GenericShellAdapter::new("bash -c \"{task}\"")
                .unwrap()
                .build_command("echo hello")
                .unwrap()
                .args,
            ["-c", "echo hello"]
        );
        for template in [
            "",
            "echo hi",
            "echo --prompt={task}",
            "{task} arg",
            "echo {task} {task}",
            "echo '",
        ] {
            assert!(GenericShellAdapter::new(template).is_err(), "{template}");
        }
    }
}
