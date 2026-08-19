# Hello plugin

The `hello` plugin is the authoring template. Its `plugin__hello__ping` tool
replies `{ "pong": true, "message": "<echo>" }` and exists so a new plugin
author can see a tool call land end to end.

Use it only when Sam is explicitly testing the plugin system. It carries no
product meaning, so never reach for it to answer a real question.
