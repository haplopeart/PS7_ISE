#[derive(Clone, Debug, Default)]
pub struct CommandInfo {
    pub name: String,
    pub module: String,
    pub command_type: String,
    pub raw: String,
}

#[derive(Default, Debug)]
pub struct CommandsModel {
    pub commands: Vec<CommandInfo>,
    pub filter: String,
    pub selected: Option<usize>,
    pub detail: String,
}

impl CommandsModel {
    pub fn load_index_payload(&mut self, payload: &str) {
        let mut cmds = Vec::new();
        for row in payload.split('\u{1e}') {
            if row.trim().is_empty() { continue; }
            let fields: Vec<&str> = row.split('\u{1f}').collect();
            let name = fields.get(0).unwrap_or(&"").to_string();
            let module = fields.get(1).unwrap_or(&"").to_string();
            let command_type = fields.get(2).unwrap_or(&"").to_string();
            cmds.push(CommandInfo { name, module, command_type, raw: row.to_string() });
        }
        cmds.sort_by(|a,b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.commands = cmds;
    }

    pub fn filtered(&self) -> Vec<(usize, &CommandInfo)> {
        let f = self.filter.to_lowercase();
        self.commands.iter().enumerate().filter(|(_,c)| f.is_empty() || c.name.to_lowercase().contains(&f) || c.module.to_lowercase().contains(&f)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_index_rows() {
        let mut m = CommandsModel::default();
        m.load_index_payload("Get-ChildItem\u{1f}Microsoft.PowerShell.Management\u{1f}Cmdlet\u{1e}Get-Content\u{1f}Microsoft.PowerShell.Management\u{1f}Cmdlet");
        assert_eq!(m.commands.len(), 2);
        assert_eq!(m.commands[0].name, "Get-ChildItem");
    }
}
