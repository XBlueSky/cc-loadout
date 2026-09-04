//! Detail sub-view for the two *pools* on the by-profile board: `Universal`
//! and `On-demand`.
//!
//! A pool is not a profile. It carries plugins but no detect rules — Universal
//! applies to every repo by definition, and On-demand is an explicit ad-hoc
//! bucket — so it has no Rules tab, no rename, and no delete. That is why it
//! cannot reuse `DetailState`, which is keyed on `working.profiles[name]`.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::profile::config::Profiles;
use crate::profile::discover::Inventory;

/// Which pool a `PoolDetailState` is editing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pool {
    Universal,
    OnDemand,
}

/// Editing state for one pool: a checkbox list over every installed plugin,
/// preselected with the pool's current members.
pub struct PoolDetailState {
    pub pool: Pool,
    pub plugins: crate::tui::multiselect::MultiSelect,
}

impl PoolDetailState {
    /// Open the given pool from `working`, using `inv` for the full plugin list.
    pub fn open(pool: Pool, inv: &Inventory, working: &Profiles) -> Self {
        let members = match pool {
            Pool::Universal => &working.universal,
            Pool::OnDemand => &working.on_demand,
        };
        let all_plugin_keys: Vec<String> = inv.plugins.iter().map(|p| p.key.clone()).collect();
        PoolDetailState {
            pool,
            plugins: crate::tui::multiselect::MultiSelect::new(all_plugin_keys, members),
        }
    }

    /// Mirror the checked set into `working` under this pool.
    ///
    /// A plugin lives in exactly ONE bucket: `by_plugin::membership` resolves
    /// universal → on_demand → profiles in that order and short-circuits, so a
    /// key left in two of them would report the wrong home (and, for on_demand,
    /// would be applied to every repo instead of being borrowed per session).
    /// Everything checked here is therefore evicted from the other two.
    fn write_back(&self, working: &mut Profiles) {
        let selected = self.plugins.selected();
        for key in &selected {
            match self.pool {
                Pool::Universal => working.on_demand.retain(|k| k != key),
                Pool::OnDemand => working.universal.retain(|k| k != key),
            }
            for p in working.profiles.values_mut() {
                p.plugins.retain(|k| k != key);
            }
        }
        match self.pool {
            Pool::Universal => working.universal = selected,
            Pool::OnDemand => working.on_demand = selected,
        }
    }

    /// Handle a key. Returns `true` when the view should return to the board.
    ///
    /// Live-save, like `DetailState`: every edit is mirrored into `working`
    /// immediately, so leaving by any means keeps the change.
    pub fn handle_key(&mut self, key: KeyEvent, working: &mut Profiles) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => true,
            _ => {
                self.plugins.on_key(key);
                self.write_back(working);
                false
            }
        }
    }
}

/// Render the pool's plugin list.
pub fn render(state: &PoolDetailState, f: &mut Frame, area: Rect) {
    let title = match state.pool {
        Pool::Universal => "Universal — plugins loaded in every repo",
        Pool::OnDemand => "On-demand — plugins borrowed per session",
    };
    state.plugins.render(f, area, title);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::Profile;
    use crate::profile::discover::PluginInfo;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn inv() -> Inventory {
        Inventory {
            plugins: vec![
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "eslint@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![],
        }
    }

    fn drawn(state: &PoolDetailState) -> String {
        let mut t = Terminal::new(TestBackend::new(60, 10)).unwrap();
        t.draw(|f| render(state, f, f.area())).unwrap();
        t.backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn universal_pool_renders_its_members_checked_and_the_rest_unchecked() {
        let working = Profiles {
            universal: vec!["serena@x".to_string()],
            ..Default::default()
        };
        let state = PoolDetailState::open(Pool::Universal, &inv(), &working);
        let text = drawn(&state);
        assert!(
            text.contains("Universal"),
            "the pool must name itself: {text}"
        );
        assert!(
            text.contains("[x] serena@x"),
            "a member renders checked: {text}"
        );
        assert!(
            text.contains("[ ] eslint@x"),
            "a non-member renders unchecked: {text}"
        );
    }

    #[test]
    fn opening_a_pool_preselects_only_that_pools_members() {
        let working = Profiles {
            universal: vec!["serena@x".to_string()],
            on_demand: vec!["eslint@x".to_string()],
            profiles: [(
                "rust".to_string(),
                Profile {
                    plugins: vec!["serena@x".to_string()],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let on_demand = PoolDetailState::open(Pool::OnDemand, &inv(), &working);
        assert_eq!(
            on_demand.plugins.selected(),
            vec!["eslint@x".to_string()],
            "the On-demand pool must preselect on_demand only, not universal or profiles"
        );
    }
}
