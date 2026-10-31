import { View, div, image, svg, PathBuilder } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, EditorState, Editor } from "gpui-base";
import { exec, props, theme } from "herdr";
import { renderTree } from "./renderer.js";

const key = f => `${f.summary.id}:${f.summary.version}`;
const button = (id, label, active, fn) => {
  const t = theme();
  return div().id(id).px(10).py(6).rounded(5).cursor_pointer().text_size(12)
    .bg(active ? t.accent : t.raised).text_color(active ? t.background : t.foreground)
    .hover(s => s.opacity(0.85)).on_click(fn).child(label);
};

export default class Easel extends View {
  init(_props, cx) {
    const p = props() ?? {};
    this.project = p.project;
    this.helper = p.helper ?? "gpui-artifacts-mod";
    this.lease = `easel-${Date.now()}-${Math.random()}`;
    this.snapshot = { frames: [], messages: [], drafts: [], markers: [] };
    this.error = null;
    this.tool = "select";
    this.showSource = false;
    this.failed = {};
    this.input = InputState.new({ placeholder: "Message, or a note for the marked element…" });
    this.input.on("submit", (_e, cx) => this.send(cx));
    if (!this.project) { this.error = "Open Easel with props: { project: an absolute project directory }."; return; }
    this.connect(cx);
  }

  async call(request) {
    const r = await exec(this.helper, ["call", this.project, JSON.stringify({ ...request, lease: this.lease })]);
    if (r.code !== 0) throw new Error(r.stderr || "Easel backend is unavailable");
    return JSON.parse(r.stdout);
  }

  connect(cx) {
    if (this.connecting) return;
    this.connecting = true;
    cx.spawn(async cx => {
      try {
        const r = await exec(this.helper, ["start", this.project]);
        if (r.code !== 0) throw new Error(r.stderr || "Could not start Easel backend");
        await this.call({ action: "open" });
        let revision = 0;
        while (!this.closed) {
          const next = await this.call({ action: "watch", revision });
          if (this.closed) break;
          if (revision !== next.revision) {
            this.snapshot = next;
            const selected = next.selected;
            if (!this.selected || !next.frames.some(f => key(f) === this.selected)) {
              this.selected = selected ? `${selected[0]}:${selected[1]}` : key(next.frames[next.frames.length - 1] ?? { summary: {} });
            } else if (next.frames.length > (this.frameCount ?? 0)) {
              this.selected = key(next.frames[next.frames.length - 1]);
            }
            this.frameCount = next.frames.length;
            this.updateSource();
            this.error = null;
            cx.notify();
          }
          revision = next.revision;
        }
      } catch (e) { if (!this.closed) { this.error = String(e); cx.notify(); } }
      finally { this.connecting = false; }
    });
  }

  action(request, cx, done) {
    cx.spawn(async cx => {
      try { await this.call(request); this.error = null; if (done) done(); }
      catch (e) { this.error = String(e); }
      cx.notify();
    });
  }

  send(cx) {
    const text = this.input.value().trim();
    if (this.marked) {
      if (!text) { this.error = "Write a note for this element, then choose Add note."; cx.notify(); return; }
      const marked = this.marked;
      this.action({ action: "annotate", ...marked, note: text }, cx, () => { this.marked = null; this.input.set_value(""); });
    } else if (text || this.snapshot.drafts.length) {
      this.action({ action: "send", text }, cx, () => this.input.set_value(""));
    }
  }

  async close() {
    this.closed = true;
    await this.call({ action: "close" });
  }

  finishDrawing(target, cx) {
    const stroke = this.stroke;
    if (!stroke) return;
    this.stroke = null;
    if (stroke.points.length > 1) {
      const points = stroke.shape === "Pen" ? stroke.points : { from: stroke.points[0], to: stroke.points[stroke.points.length - 1] };
      this.action({ action: "annotate", ...target, kind: "drawing", shape: stroke.shape, points }, cx);
    }
    cx.notify();
  }

  drawing(shape) {
    const name = Object.keys(shape)[0], value = shape[name];
    const p = PathBuilder.stroke(2);
    if (name === "Pen") {
      if (!value.length) return div();
      p.move_to(value[0].x, value[0].y);
      for (const point of value.slice(1)) p.line_to(point.x, point.y);
    } else {
      const { from, to } = value;
      p.move_to(from.x, from.y);
      if (name === "Rect") p.line_to(to.x, from.y).line_to(to.x, to.y).line_to(from.x, to.y).close();
      else {
        p.line_to(to.x, to.y);
        const angle = Math.atan2(to.y - from.y, to.x - from.x);
        p.move_to(to.x - 10 * Math.cos(angle - 0.5), to.y - 10 * Math.sin(angle - 0.5))
          .line_to(to.x, to.y).line_to(to.x - 10 * Math.cos(angle + 0.5), to.y - 10 * Math.sin(angle + 0.5));
      }
    }
    return window.paint_path(p.build(), theme().accent).absolute().inset_0();
  }

  updateSource() {
    if (!this.showSource) return;
    const frames = this.snapshot.frames;
    const f = frames.find(f => key(f) === this.selected) ?? frames[frames.length - 1];
    if (f && this.sourceKey !== key(f)) {
      this.sourceKey = key(f);
      if (this.code) this.code.set_value(f.source);
      else this.code = EditorState.new({ language: "rust", value: f.source, readonly: true });
    }
  }

  frame(cx) {
    const t = theme(), frames = this.snapshot.frames;
    const f = frames.find(f => key(f) === this.selected) ?? frames[frames.length - 1];
    if (!f) return div().p(30).text_color(t.muted).child("Render GPUI from your agent to start a canvas here.");
    const s = f.summary, target = { id: s.id, version: s.version };
    const status = s.status;
    if (this.showSource) {
      return this.code ? Editor.new(this.code).w_full().min_h(300) : div();
    }
    let preview;
    if (f.tree && !this.failed[key(f)]) {
      try {
        preview = renderTree(f.tree, { div, image, svg }, this.project, (path, event, cx) => {
          if (this.tool !== "marker") return false;
          const origin = this.previewBounds;
          const pin = origin ? { x: event.position.x - origin.x, y: event.position.y - origin.y } : { x: 0, y: 0 };
          this.marked = { ...target, path, pin };
          this.input.focus(); cx.notify(); return true;
        }, [], key(f));
      } catch (e) {
        this.failed[key(f)] = String(e);
        this.action({ action: "render_error", ...target, message: String(e) }, cx);
      }
    }
    const box = div().id(`frame-${key(f)}`).relative().flex_none().bg(t.background).text_color(t.foreground).text_size(16)
      .on_mouse_move(event => { this.previewBounds = event.bounds; });
    if (f.frame.width) box.w(f.frame.width); else box.min_w(300);
    if (f.frame.height) box.h(f.frame.height);
    if (preview !== undefined) {
      box.child(preview);
      for (const d of (this.snapshot.drawings ?? []).filter(d => d.frame[0] === s.id && d.frame[1] === s.version)) box.child(this.drawing(d.shape));
      if (this.stroke?.points.length > 1) {
        const points = this.stroke.points;
        box.child(this.drawing({ [this.stroke.shape]: this.stroke.shape === "Pen" ? points : { from: points[0], to: points[points.length - 1] } }));
      }
      if (["Pen", "Arrow", "Rect"].includes(this.tool)) box.child(div().id("drawing-input").absolute().inset_0().cursor_crosshair()
        .on_mouse_down("left", (e, cx) => { this.stroke = { shape: this.tool, points: [e.local_position] }; cx.stop_propagation(); cx.notify(); })
        .on_mouse_move((e, cx) => { if (this.stroke && e.local_position) { this.stroke.points.push(e.local_position); cx.notify(); } })
        .on_mouse_up("left", (_e, cx) => this.finishDrawing(target, cx))
        .on_hover((hovered, cx) => { if (!hovered) this.finishDrawing(target, cx); }));
    }
    else {
      box.p(20).max_w(720).child(div().mb(8).font_semibold().child(status.state.replace(/_/g, " ")));
      const reasons = status.reasons ?? status.diagnostics ?? status.errors ?? [];
      for (const reason of reasons) box.child(div().mb(8).text_size(12).child(reason.message));
      if (status.stderr_tail) box.child(div().text_size(12).child(status.stderr_tail));
      if (this.failed[key(f)] && status.state === "rendered") box.child(String(this.failed[key(f)]));
      if (status.state === "awaiting_approval") {
        box.child(div().my(10).text_size(12).text_color(t.muted).child("Compile and run this Rust code in a separate native window?"));
        box.child(h_flex().gap(8)
          .child(button("approve", "Compile and run", false, (_e, cx) => this.action({ action: "approve", ...target }, cx)))
          .child(button("decline", "Decline", false, (_e, cx) => this.action({ action: "decline", ...target }, cx))));
      }
    }
    return v_flex().gap(10).p(20).flex_none()
      .child(h_flex().gap(10).items_center().child(div().font_semibold().child(s.title)).child(div().text_color(t.muted).child(`v${s.version}`)))
      .child(box)
      .children(this.snapshot.markers.filter(m => m.frame[0] === s.id && m.frame[1] === s.version).map(m =>
        div().text_size(12).text_color(t.accent).child(`${m.number}. ${m.anchor?.text ? m.anchor.text.slice(0, 80) + " · " : ""}${m.note}`)))
      .children((this.snapshot.notes ?? []).filter(n => n.near?.[0] === s.id && n.near?.[1] === s.version).map(n =>
        div().p(12).rounded(5).border_1().border_color(t.accent).text_size(12).child(`${n.number}. ${n.text}`)));
  }

  render(cx) {
    const t = theme();
    const frames = this.snapshot.frames;
    const root = v_flex().size_full().min_w(0).min_h(0).bg(t.surface).text_color(t.foreground).text_size(13);
    root.child(h_flex().flex_none().px(12).py(10).gap(8).items_center().border_b_1().border_color(t.border)
      .child(div().font_semibold().child("Easel"))
      .child(div().flex_1().truncate().text_color(t.muted).child(this.project ?? ""))
      .child(button("select-tool", "Select", this.tool === "select", (_e, cx) => { this.tool = "select"; this.marked = null; cx.notify(); }))
      .child(button("marker-tool", "Mark", this.tool === "marker", (_e, cx) => { this.tool = "marker"; cx.notify(); })));
    root.child(h_flex().flex_none().gap(6).px(12).py(6)
      .children([ ["Pen", "Pen"], ["Arrow", "Arrow"], ["Rect", "Box"] ].map(([tool, label]) =>
        button(`tool-${tool}`, label, this.tool === tool, (_e, cx) => { this.tool = tool; this.marked = null; cx.notify(); })))
      .child(button("note", "Note", false, (_e, cx) => {
        const f = frames.find(f => key(f) === this.selected);
        if (f) { this.marked = { id: f.summary.id, version: f.summary.version, kind: "note" }; this.input.focus(); cx.notify(); }
      }))
      .child(button("source", this.showSource ? "Preview" : "Source", this.showSource, (_e, cx) => { this.showSource = !this.showSource; this.updateSource(); cx.notify(); })));
    if (this.error) root.child(h_flex().p(10).gap(8).items_center()
      .child(div().flex_1().child(this.error))
      .child(button("retry", "Reconnect", false, (_e, cx) => this.connect(cx))));
    if (frames.length) root.child(h_flex().id("versions").flex_none().gap(6).px(12).py(8).overflow_x_scroll()
      .children(frames.map(f => button(`version-${key(f)}`, `${f.summary.title} · v${f.summary.version}`, key(f) === this.selected, (_e, cx) => {
        this.selected = key(f); this.marked = null;
        this.updateSource();
        this.action({ action: "select", id: f.summary.id, version: f.summary.version }, cx); cx.notify();
      }))));
    root.child(div().id("canvas").flex_1().min_h(0).overflow_scroll().child(this.frame(cx)));
    root.child(v_flex().id("chat").flex_none().max_h(190).overflow_y_scroll().gap(8).px(12).py(8).border_t_1().border_color(t.border)
      .children(this.snapshot.messages.map(m => v_flex().gap(3)
        .child(div().text_size(11).text_color(t.muted).child(m.author === "user" ? "You" : m.author === "claude" ? "Agent" : "Easel"))
        .child(div().child(m.text))
        .children((m.attachments ?? []).map(a => div().text_size(11).text_color(t.muted).child(`${a.number}. ${a.note ?? a.text ?? a.shape ?? "annotation"}`))))));
    if (this.snapshot.drafts.length) root.child(h_flex().id("drafts").flex_none().gap(6).px(12).py(6).overflow_x_scroll()
      .children(this.snapshot.drafts.map(a => button(`draft-${a.number}`, `${a.number}. ${a.note ?? a.text ?? "annotation"} ×`, false,
        (_e, cx) => this.action({ action: "remove_annotation", number: a.number }, cx)))));
    if (this.marked) root.child(h_flex().px(12).gap(8).items_center()
      .child(div().flex_1().text_size(11).text_color(t.accent).child(`Note on ${this.marked.id} v${this.marked.version}${this.marked.path ? " · element " + (this.marked.path.join(".") || "root") : ""}`))
      .child(button("cancel-note", "Cancel", false, (_e, cx) => { this.marked = null; cx.notify(); })));
    root.child(h_flex().flex_none().gap(8).p(12)
      .child(Input.new(this.input).flex_1().h(36).px(10).rounded(5).bg(t.background).border_1().border_color(t.border))
      .child(button("send", this.marked ? "Add note" : "Send", false, (_e, cx) => this.send(cx))));
    return root;
  }
}
