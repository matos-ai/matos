@id("deny-human-kind-with-actor")
forbid (
  principal,
  action in Matos::Action::"matos.any",
  resource
)
when {
  context has actor && context.actor_kind == "human"
};
