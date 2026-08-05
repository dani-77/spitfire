# Attributions

The code in `crates/spitfire/src/` (`state.rs`, `winit.rs`, `input_handler.rs`,
`focus.rs`, `render.rs`, `drawing.rs`, `shell/`) was adapted from the
[`anvil`](https://github.com/Smithay/smithay/tree/v0.7.0/anvil) example, distributed
with the [Smithay](https://github.com/Smithay/smithay) project under the MIT license:

```
MIT License

Copyright (c) 2017 Victor Berger and Victoria Brekenfeld

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

`crates/spitfire/src/ext_workspace.rs` (the hand-written `ext-workspace-v1` server
implementation, Phase 5 — this protocol isn't provided by Smithay itself) follows the
same broadcast-on-bind/full-resync architecture as Smithay's own
`wayland::foreign_toplevel_list` module (a same-shaped protocol: a manager that pushes
`new_id` objects to every bound client as compositor state changes over time), also
MIT/Apache-2.0 licensed. No code was copied — the domain logic (workspaces, not
toplevels; a 3-level object hierarchy instead of 2; client requests that mutate
compositor state) is new — but the shape is directly inspired by it, so it's credited
here too.
