use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Selection {
    directory: String,
    pub chosen: BTreeSet<String>,
    anchor: Option<String>,
}
impl Selection {
    pub fn clear(&mut self) {
        self.chosen.clear();
        self.anchor = None;
    }
    pub fn sync(&mut self, directory: &str, visible: &[String]) {
        if self.directory != directory {
            self.clear();
            self.directory = directory.into();
        }
        self.chosen.retain(|k| visible.contains(k));
        if self.anchor.as_ref().is_some_and(|k| !visible.contains(k)) {
            self.anchor = None;
        }
    }
    pub fn all(&mut self, visible: &[String]) {
        self.chosen = visible.iter().cloned().collect();
        self.anchor = visible.first().cloned();
    }
    pub fn toggle_all(&mut self, visible: &[String]) {
        if !visible.is_empty() && visible.iter().all(|k| self.chosen.contains(k)) {
            self.clear();
        } else {
            self.all(visible);
        }
    }
    pub fn click(
        &mut self,
        key: &str,
        visible: &[String],
        ctrl: bool,
        shift: bool,
        checkbox: bool,
    ) {
        if shift
            && let Some(a) = self
                .anchor
                .as_ref()
                .and_then(|k| visible.iter().position(|v| v == k))
            && let Some(b) = visible.iter().position(|v| v == key)
        {
            if !ctrl {
                self.chosen.clear();
            }
            self.chosen
                .extend(visible[a.min(b)..=a.max(b)].iter().cloned());
            return;
        }
        if ctrl || checkbox {
            if !self.chosen.remove(key) {
                self.chosen.insert(key.into());
            }
        } else {
            self.chosen = BTreeSet::from([key.into()]);
        }
        self.anchor = Some(key.into());
    }
    pub fn context(&mut self, key: &str) {
        if !self.chosen.contains(key) {
            self.chosen = BTreeSet::from([key.into()]);
            self.anchor = Some(key.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_toggles_and_refresh_use_paths_not_row_positions() {
        let keys = ["a", "b", "c", "d"].map(str::to_owned);
        let mut s = Selection::default();
        s.sync("one", &keys);
        s.click("b", &keys, false, false, false);
        s.click("d", &keys, false, true, false);
        assert_eq!(s.chosen, ["b", "c", "d"].map(str::to_owned).into());
        s.click("c", &keys, false, false, true);
        assert_eq!(s.chosen, ["b", "d"].map(str::to_owned).into());
        s.click("a", &keys, true, false, false);
        s.context("d");
        assert_eq!(s.chosen.len(), 3);
        s.sync("one", &["d".into(), "a".into()]);
        assert_eq!(s.chosen, ["a", "d"].map(str::to_owned).into());
        s.sync("two", &keys);
        assert!(s.chosen.is_empty());
        s.toggle_all(&keys);
        assert_eq!(s.chosen.len(), 4);
        s.toggle_all(&keys);
        assert!(s.chosen.is_empty());
    }
}
