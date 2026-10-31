// Translate Easel's evaluated IR into gpui-kit builders. Never evaluate source
// as JavaScript. Unknown bridge methods fail visibly instead of disappearing.
const WEIGHTS = { THIN: 100, EXTRA_LIGHT: 200, LIGHT: 300, NORMAL: 400, MEDIUM: 500, SEMIBOLD: 600, BOLD: 700, EXTRA_BOLD: 800, BLACK: 900 };
const NAMED = { red: [255, 0, 0, 255], green: [0, 255, 0, 255], blue: [0, 0, 255, 255], yellow: [255, 255, 0, 255], black: [0, 0, 0, 255], white: [255, 255, 255, 255], transparent_black: [0, 0, 0, 0], transparent_white: [255, 255, 255, 0] };

export function rgba(color) {
  if (color.rgb !== undefined) return [(color.rgb >>> 16) & 255, (color.rgb >>> 8) & 255, color.rgb & 255, 255];
  if (color.rgba !== undefined) return [(color.rgba >>> 24) & 255, (color.rgba >>> 16) & 255, (color.rgba >>> 8) & 255, color.rgba & 255];
  if (color.named) {
    if (!NAMED[color.named]) throw new Error(`Unknown color ${color.named}`);
    return [...NAMED[color.named]];
  }
  if (color.opacity || color.alpha) {
    const v = color.opacity ?? color.alpha, channels = rgba(v.base);
    channels[3] = color.opacity ? channels[3] * v.factor : v.alpha * 255;
    return channels;
  }
  if (color.hsla) {
    const { h, s, l, a } = color.hsla;
    const chroma = (1 - Math.abs(2 * l - 1)) * s;
    const hue = ((h % 1) + 1) % 1 * 6;
    const x = chroma * (1 - Math.abs(hue % 2 - 1)), m = l - chroma / 2;
    const [r, g, b] = hue < 1 ? [chroma, x, 0] : hue < 2 ? [x, chroma, 0] : hue < 3 ? [0, chroma, x] : hue < 4 ? [0, x, chroma] : hue < 5 ? [x, 0, chroma] : [chroma, 0, x];
    return [(r + m) * 255, (g + m) * 255, (b + m) * 255, a * 255];
  }
  throw new Error("Unknown color representation");
}

export function colorString(color) {
  return "#" + rgba(color).map(c => Math.round(Math.max(0, Math.min(255, c))).toString(16).padStart(2, "0")).join("");
}

export function argument(arg) {
  const v = arg.value;
  switch (arg.type) {
    case "color": return colorString(v);
    case "length":
      if (v === "auto") return "auto";
      if (v.px !== undefined) return `${v.px}px`;
      if (v.rems !== undefined) return `${v.rems}rem`;
      if (v.relative !== undefined) return `${v.relative * 100}%`;
      throw new Error("Unknown length representation");
    case "number": case "str": case "bool": return v;
    case "variant":
      if (v[0] === "FontWeight" && WEIGHTS[v[1]] !== undefined) return WEIGHTS[v[1]];
      return v[1].replace(/([a-z])([A-Z])/g, "$1_$2").toLowerCase();
    case "style": return el => applyCalls(el, v);
    // The bridge does not expose GPUI's Vec<BoxShadow> builder. Preserve the
    // failure so Auto can offer native compilation with explicit approval.
    case "shadows": throw new Error("Custom BoxShadow arrays require a compiled preview in this mod");
    default: throw new Error(`Unknown IR argument ${arg.type}`);
  }
}

export function applyCalls(element, calls) {
  for (const call of calls) {
    // svg(path) already consumed the constructor's path below.
    if (call.method === "path") continue;
    if (typeof element[call.method] !== "function") throw new Error(`Herdr's mod renderer does not expose .${call.method}()`);
    element = element[call.method](...call.args.map(argument));
  }
  return element;
}

function resolve(source, project) {
  return source.startsWith("/") || /^[a-z][a-z0-9+.-]*:/i.test(source) ? source : `${project}/${source}`;
}

export function renderTree(node, kit, project, onMarker, path = [], prefix = "preview") {
  if (node.node === "text") return node.text;
  const kind = node.kind;
  let el;
  if (kind.kind === "div") el = kit.div();
  else if (kind.kind === "img") el = kit.image(resolve(kind.source, project));
  else if (kind.kind === "svg") {
    const p = node.calls.find(c => c.method === "path")?.args[0]?.value;
    if (!p) throw new Error("svg() requires a path");
    el = kit.svg(resolve(p, project));
  } else throw new Error(`Unknown element ${kind.kind}`);
  // Scope source ids to a frame: two versions can contain the same id.
  const calls = node.calls.map(c => c.method === "id" || c.method === "group" ? { ...c, args: [{ type: "str", value: `${prefix}-${c.args[0].value}` }] } :
    c.method === "group_hover" || c.method === "group_active" ? { ...c, args: [{ type: "str", value: `${prefix}-${c.args[0].value}` }, ...c.args.slice(1)] } : c);
  el = applyCalls(el, calls);
  if (node.children.length) el = el.children(node.children.map((child, i) => renderTree(child, kit, project, onMarker, [...path, i], prefix)));
  if (onMarker && kind.kind === "div") {
    if (!calls.some(c => c.method === "id")) el = el.id(`${prefix}-node-${path.join("-")}`);
    el = el.on_mouse_down("left", (event, cx) => {
      if (onMarker(path, event, cx)) cx.stop_propagation();
    });
  }
  return el;
}
