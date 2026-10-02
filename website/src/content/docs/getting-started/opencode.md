---
title: OpenCode setup
description: Wire up the OpenCode plugin bridge from the bundled plugin directory.
---

OpenCode uses the bundled plugin bridge. Once you make the plugin visible to
OpenCode, the sidebar can receive its events automatically.

## Plugin bridge

### Link the plugin file

Create OpenCode's global plugin directory if it does not already exist, then
symlink the plugin **file** (not the directory) into it. Linking the single
file lets the bridge coexist with any other plugins you have installed:

```sh
mkdir -p ~/.config/opencode/plugins
ln -sf ~/.tmux/plugins/herdmux/.opencode/plugins/herdmux.js \
  ~/.config/opencode/plugins/herdmux.js
```

If you keep `herdmux` in a different path, point the symlink at
that copy instead.

### Restart OpenCode

Restart OpenCode after adding the plugin so it discovers the new bridge.
