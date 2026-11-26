@id("deny-mcp-call-without-initiator-and-grant")
forbid (
  principal,
  action == Matos::Action::"mcp.call",
  resource
)
unless {
  context.initiated && context.granted
};
