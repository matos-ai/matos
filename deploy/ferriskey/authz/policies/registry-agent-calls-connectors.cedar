@id("registry-agent-calls-connectors")
permit (
  principal,
  action == Matos::Action::"mcp.call",
  resource
)
when {
  context.actor_kind == "registry" && context has actor &&
  context.organization == resource.organization &&
  context.initiated &&
  context.granted
};
