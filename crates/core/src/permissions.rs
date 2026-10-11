//! What an account may do on a server (`docs/notes/permissions.md`).
//!
//! # A closed list, defined once
//!
//! Every decision the simulator and atlas make about "may this account do
//! that?" names one of these. The list lives here, in the crate every side
//! depends on, so the simulator that enforces a permission, atlas that asks for
//! one and the web UI that grants one cannot disagree about what exists — the web
//! UI reads it from the server (`GET /api/admin/permissions`) rather than keeping
//! a copy. A permission the code does not check is not in the list, and a slug
//! the list does not hold is refused when a role is saved, for the reason the
//! placeholder kinds are closed: a grant that silently does nothing is worse than
//! one that was refused.
//!
//! # Roles, and the two that are built in
//!
//! A role is a named set of permissions. Every enabled account has the built-in
//! **player** role, whose set an operator edits like any other; an account may
//! hold any number of further roles; what it may do is the union. **Admin** is
//! the account's `admin` flag and holds every permission, including ones added
//! later — it is not a set somebody could forget to extend, and it keeps the rule
//! that the last enabled admin cannot be demoted.

use serde::{Deserialize, Serialize};

/// One thing an account may be allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    // ── Prims ──
    /// Build prims.
    PrimCreate,
    /// Change and delete the prims this account built.
    PrimEditOwn,
    /// Change and delete anybody's prims, and the server's own.
    PrimEditAny,
    /// Make a prim physical (`SetPrimPhysical`, `set_physical`).
    PrimPhysical,
    /// Play a video on a prim.
    PrimVideo,
    // ── Scripts ──
    /// Write the scripts on this account's own prims.
    ScriptEditOwn,
    /// Write the script on anybody's prim.
    ScriptEditAny,
    /// Read the script on a prim somebody else built.
    ScriptReadAny,
    // ── Buildings ──
    /// Take ownership of a building.
    BuildingClaim,
    /// Draw the inside of a building this account owns.
    BuildingDesignOwn,
    /// Draw the inside of any building.
    BuildingDesignAny,
    /// Replace a building's outside with a model (`docs/notes/building-models.md`).
    BuildingOverride,
    // ── Running the server ──
    /// Install content packs and author prims through `/api/admin/prims`.
    ContentInstall,
    /// Curate the shared model library, placeholder models and stored assets.
    LibraryManage,
    /// Create, change, disable and delete accounts.
    AccountsManage,
    /// Edit roles and who holds them.
    RolesManage,
    /// Read and resolve reports, and release building claims.
    ReportsTriage,
    /// Run atlas's data pipelines.
    PipelineRun,
}

/// How the web UI groups the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionGroup {
    Prims,
    Scripts,
    Buildings,
    Server,
}

/// Every permission, in the order the web UI lists them.
pub const ALL: &[Permission] = &[
    Permission::PrimCreate,
    Permission::PrimEditOwn,
    Permission::PrimEditAny,
    Permission::PrimPhysical,
    Permission::PrimVideo,
    Permission::ScriptEditOwn,
    Permission::ScriptEditAny,
    Permission::ScriptReadAny,
    Permission::BuildingClaim,
    Permission::BuildingDesignOwn,
    Permission::BuildingDesignAny,
    Permission::BuildingOverride,
    Permission::ContentInstall,
    Permission::LibraryManage,
    Permission::AccountsManage,
    Permission::RolesManage,
    Permission::ReportsTriage,
    Permission::PipelineRun,
];

impl Permission {
    /// The name it is stored and sent under.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::PrimCreate => "prim_create",
            Self::PrimEditOwn => "prim_edit_own",
            Self::PrimEditAny => "prim_edit_any",
            Self::PrimPhysical => "prim_physical",
            Self::PrimVideo => "prim_video",
            Self::ScriptEditOwn => "script_edit_own",
            Self::ScriptEditAny => "script_edit_any",
            Self::ScriptReadAny => "script_read_any",
            Self::BuildingClaim => "building_claim",
            Self::BuildingDesignOwn => "building_design_own",
            Self::BuildingDesignAny => "building_design_any",
            Self::BuildingOverride => "building_override",
            Self::ContentInstall => "content_install",
            Self::LibraryManage => "library_manage",
            Self::AccountsManage => "accounts_manage",
            Self::RolesManage => "roles_manage",
            Self::ReportsTriage => "reports_triage",
            Self::PipelineRun => "pipeline_run",
        }
    }

    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        ALL.iter().copied().find(|p| p.slug() == slug)
    }

    /// One line for the web UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PrimCreate => "Build prims",
            Self::PrimEditOwn => "Change and delete their own prims",
            Self::PrimEditAny => "Change and delete anybody's prims",
            Self::PrimPhysical => "Make prims physical",
            Self::PrimVideo => "Play videos on prims",
            Self::ScriptEditOwn => "Script their own prims",
            Self::ScriptEditAny => "Script anybody's prims",
            Self::ScriptReadAny => "Read the scripts on other people's prims",
            Self::BuildingClaim => "Take ownership of a building",
            Self::BuildingDesignOwn => "Draw the inside of their own buildings",
            Self::BuildingDesignAny => "Draw the inside of any building",
            Self::BuildingOverride => "Replace a building's outside with a model",
            Self::ContentInstall => "Install content packs and author prims",
            Self::LibraryManage => "Curate the model library",
            Self::AccountsManage => "Manage accounts",
            Self::RolesManage => "Manage roles",
            Self::ReportsTriage => "Handle reports and building claims",
            Self::PipelineRun => "Run the map data pipelines",
        }
    }

    #[must_use]
    pub fn group(self) -> PermissionGroup {
        match self {
            Self::PrimCreate
            | Self::PrimEditOwn
            | Self::PrimEditAny
            | Self::PrimPhysical
            | Self::PrimVideo => PermissionGroup::Prims,
            Self::ScriptEditOwn | Self::ScriptEditAny | Self::ScriptReadAny => {
                PermissionGroup::Scripts
            }
            Self::BuildingClaim
            | Self::BuildingDesignOwn
            | Self::BuildingDesignAny
            | Self::BuildingOverride => PermissionGroup::Buildings,
            Self::ContentInstall
            | Self::LibraryManage
            | Self::AccountsManage
            | Self::RolesManage
            | Self::ReportsTriage
            | Self::PipelineRun => PermissionGroup::Server,
        }
    }

    /// What the built-in player role starts with on a new server — exactly what
    /// every signed-in account could do before roles existed, so introducing them
    /// changes nobody's abilities until an operator decides it should.
    #[must_use]
    pub fn player_default(self) -> bool {
        matches!(
            self,
            Self::PrimCreate
                | Self::PrimEditOwn
                | Self::PrimPhysical
                | Self::PrimVideo
                | Self::ScriptEditOwn
                | Self::ScriptReadAny
                | Self::BuildingClaim
                | Self::BuildingDesignOwn
        )
    }

    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// A set of permissions — what one account holds once its roles are added up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionSet(u32);

impl PermissionSet {
    /// Every permission: an admin.
    #[must_use]
    pub fn all() -> Self {
        Self(ALL.iter().fold(0, |m, p| m | p.bit()))
    }

    #[must_use]
    pub fn none() -> Self {
        Self(0)
    }

    #[must_use]
    pub fn has(self, p: Permission) -> bool {
        self.0 & p.bit() != 0
    }

    pub fn insert(&mut self, p: Permission) {
        self.0 |= p.bit();
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The members, in [`ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Permission> {
        ALL.iter().copied().filter(move |p| self.has(*p))
    }

    /// The slugs, for an answer over HTTP.
    #[must_use]
    pub fn slugs(self) -> Vec<&'static str> {
        self.iter().map(Permission::slug).collect()
    }
}

impl FromIterator<Permission> for PermissionSet {
    fn from_iter<I: IntoIterator<Item = Permission>>(iter: I) -> Self {
        let mut s = Self::none();
        for p in iter {
            s.insert(p);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn slugs_are_unique_and_round_trip() {
        let mut seen = HashSet::new();
        for p in ALL {
            assert!(seen.insert(p.slug()), "{} is listed twice", p.slug());
            assert_eq!(Permission::from_slug(p.slug()), Some(*p));
            // The serde name and the slug are one name.
            assert_eq!(
                serde_json::to_string(p).unwrap(),
                format!("\"{}\"", p.slug())
            );
        }
        assert_eq!(Permission::from_slug("prim_teleport"), None);
    }

    /// The bitset must hold every permission; adding a twentieth is fine, a
    /// thirty-third is not.
    #[test]
    fn every_permission_fits_the_set() {
        assert!(ALL.len() <= 32);
        let all = PermissionSet::all();
        assert_eq!(all.iter().count(), ALL.len());
        assert!(ALL.iter().all(|p| all.has(*p)));
    }

    /// The enum order and the list order are the same, so `as u32` is the
    /// position and nothing is missing from `ALL`.
    #[test]
    fn the_list_is_the_enum_in_order() {
        for (i, p) in ALL.iter().enumerate() {
            assert_eq!(*p as usize, i, "{} is out of place", p.slug());
        }
    }

    /// A player holds nothing that runs the server.
    #[test]
    fn the_player_default_is_no_operator() {
        for p in ALL.iter().filter(|p| p.player_default()) {
            assert_ne!(p.group(), PermissionGroup::Server, "{}", p.slug());
        }
    }
}
