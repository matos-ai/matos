@id("deny-admin-action-to-non-admin")
forbid (
  principal,
  action in [
    Matos::Action::"provider.manage",
    Matos::Action::"models.assign",
    Matos::Action::"github.manage",
    Matos::Action::"integration.manage",
    Matos::Action::"budget.manage"
  ],
  resource
)
when {
  !context.is_admin
};
