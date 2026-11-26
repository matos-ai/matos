@id("deny-non-human-without-actor")
forbid (
  principal,
  action in Matos::Action::"matos.any",
  resource
)
when {
  context.actor_kind != "human" && !(context has actor)
};
