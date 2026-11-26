@id("deny-cross-organization")
forbid (
  principal,
  action in Matos::Action::"matos.any",
  resource
)
when {
  context.organization != resource.organization
};
