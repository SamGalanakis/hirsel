// Lucide SVG artwork, ISC license. See lucide-LICENSE.txt beside this file.
import { For, omit } from "solid-js";
import { Dynamic, type JSX } from "@solidjs/web";
type IconProps = JSX.SvgSVGAttributes<SVGSVGElement> & { size?: number | string; strokeWidth?: number | string };
type IconNode = [string, Record<string, string>];
function Icon(props: IconProps & { nodes: IconNode[] }) {
  const attributes = omit(props, "nodes", "size", "strokeWidth");
  return <svg xmlns="http://www.w3.org/2000/svg" width={props.size ?? 24} height={props.size ?? 24} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width={props.strokeWidth ?? 2} stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {...attributes}><For each={props.nodes}>{([tag, attrs]) => <Dynamic component={tag} {...attrs} />}</For></svg>;
}
export const Activity = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2",
      key: "169zse"
    }
  ]
]} />;
export const Archive = (props: IconProps) => <Icon {...props} nodes={[
  ["rect", { width: "20", height: "5", x: "2", y: "3", rx: "1" }],
  ["path", { d: "M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8" }],
  ["path", { d: "M10 12h4" }]
]} />;
export const ArrowDownToLine = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M12 17V3" }],
  ["path", { d: "m6 11 6 6 6-6" }],
  ["path", { d: "M19 21H5" }]
]} />;
export const ArrowUp = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "m5 12 7-7 7 7" }],
  ["path", { d: "M12 19V5" }]
]} />;
export const Bot = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M12 8V4H8" }],
  ["rect", { width: "16", height: "12", x: "4", y: "8", rx: "2" }],
  ["path", { d: "M2 14h2" }],
  ["path", { d: "M20 14h2" }],
  ["path", { d: "M15 13v2" }],
  ["path", { d: "M9 13v2" }]
]} />;
export const Braces = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    { d: "M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5c0 1.1.9 2 2 2h1" }
  ],
  [
    "path",
    {
      d: "M16 21h1a2 2 0 0 0 2-2v-5c0-1.1.9-2 2-2a2 2 0 0 1-2-2V5a2 2 0 0 0-2-2h-1",
      key: "e1hn23"
    }
  ]
]} />;
export const Brain = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M12 18V5" }],
  ["path", { d: "M15 13a4.17 4.17 0 0 1-3-4 4.17 4.17 0 0 1-3 4" }],
  ["path", { d: "M17.598 6.5A3 3 0 1 0 12 5a3 3 0 1 0-5.598 1.5" }],
  ["path", { d: "M17.997 5.125a4 4 0 0 1 2.526 5.77" }],
  ["path", { d: "M18 18a4 4 0 0 0 2-7.464" }],
  ["path", { d: "M19.967 17.483A4 4 0 1 1 12 18a4 4 0 1 1-7.967-.517" }],
  ["path", { d: "M6 18a4 4 0 0 1-2-7.464" }],
  ["path", { d: "M6.003 5.125a4 4 0 0 0-2.526 5.77" }]
]} />;
export const Check = (props: IconProps) => <Icon {...props} nodes={[["path", { d: "M20 6 9 17l-5-5" }]]} />;
export const ChevronDown = (props: IconProps) => <Icon {...props} nodes={[["path", { d: "m6 9 6 6 6-6" }]]} />;
export const ChevronLeft = (props: IconProps) => <Icon {...props} nodes={[["path", { d: "m15 18-6-6 6-6" }]]} />;
export const ChevronRight = (props: IconProps) => <Icon {...props} nodes={[["path", { d: "m9 18 6-6-6-6" }]]} />;
export const CircleStop = (props: IconProps) => <Icon {...props} nodes={[
  ["circle", { cx: "12", cy: "12", r: "10" }],
  ["rect", { x: "9", y: "9", width: "6", height: "6", rx: "1" }]
]} />;
export const Clock = (props: IconProps) => <Icon {...props} nodes={[
  ["circle", { cx: "12", cy: "12", r: "10" }],
  ["path", { d: "M12 6v6l4 2" }]
]} />;
export const Copy = (props: IconProps) => <Icon {...props} nodes={[
  ["rect", { width: "14", height: "14", x: "8", y: "8", rx: "2", ry: "2" }],
  ["path", { d: "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" }]
]} />;
export const CornerDownLeft = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M20 4v7a4 4 0 0 1-4 4H4" }],
  ["path", { d: "m9 10-5 5 5 5" }]
]} />;
export const File = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z",
      key: "1oefj6"
    }
  ],
  ["path", { d: "M14 2v5a1 1 0 0 0 1 1h5" }]
]} />;
export const FileText = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z",
      key: "1oefj6"
    }
  ],
  ["path", { d: "M14 2v5a1 1 0 0 0 1 1h5" }],
  ["path", { d: "M10 9H8" }],
  ["path", { d: "M16 13H8" }],
  ["path", { d: "M16 17H8" }]
]} />;
export const Layers = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z",
      key: "zw3jo"
    }
  ],
  [
    "path",
    {
      d: "M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12",
      key: "1wduqc"
    }
  ],
  [
    "path",
    {
      d: "M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17",
      key: "kqbvx6"
    }
  ]
]} />;
export const ListTree = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M8 5h13" }],
  ["path", { d: "M13 12h8" }],
  ["path", { d: "M13 19h8" }],
  ["path", { d: "M3 10a2 2 0 0 0 2 2h3" }],
  ["path", { d: "M3 5v12a2 2 0 0 0 2 2h3" }]
]} />;
export const LoaderCircle = (props: IconProps) => <Icon {...props} nodes={[["path", { d: "M21 12a9 9 0 1 1-6.219-8.56" }]]} />;
export const Maximize2 = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M15 3h6v6" }],
  ["path", { d: "m21 3-7 7" }],
  ["path", { d: "m3 21 7-7" }],
  ["path", { d: "M9 21H3v-6" }]
]} />;
export const MessagesSquare = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "M16 10a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 14.286V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z",
      key: "1n2ejm"
    }
  ],
  [
    "path",
    {
      d: "M20 9a2 2 0 0 1 2 2v10.286a.71.71 0 0 1-1.212.502l-2.202-2.202A2 2 0 0 0 17.172 19H10a2 2 0 0 1-2-2v-1",
      key: "1qfcsi"
    }
  ]
]} />;
export const Minimize2 = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "m14 10 7-7" }],
  ["path", { d: "M20 10h-6V4" }],
  ["path", { d: "m3 21 7-7" }],
  ["path", { d: "M4 14h6v6" }]
]} />;
export const MoreHorizontal = (props: IconProps) => <Icon {...props} nodes={[
  ["circle", { cx: "12", cy: "12", r: "1" }],
  ["circle", { cx: "19", cy: "12", r: "1" }],
  ["circle", { cx: "5", cy: "12", r: "1" }]
]} />;
export const OctagonX = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "m15 9-6 6" }],
  [
    "path",
    {
      d: "M2.586 16.726A2 2 0 0 1 2 15.312V8.688a2 2 0 0 1 .586-1.414l4.688-4.688A2 2 0 0 1 8.688 2h6.624a2 2 0 0 1 1.414.586l4.688 4.688A2 2 0 0 1 22 8.688v6.624a2 2 0 0 1-.586 1.414l-4.688 4.688a2 2 0 0 1-1.414.586H8.688a2 2 0 0 1-1.414-.586z",
      key: "2d38gg"
    }
  ],
  ["path", { d: "m9 9 6 6" }]
]} />;
export const PanelRight = (props: IconProps) => <Icon {...props} nodes={[
  ["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }],
  ["path", { d: "M15 3v18" }]
]} />;
export const Paperclip = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "m16 6-8.414 8.586a2 2 0 0 0 2.829 2.829l8.414-8.586a4 4 0 1 0-5.657-5.657l-8.379 8.551a6 6 0 1 0 8.485 8.485l8.379-8.551",
      key: "1miecu"
    }
  ]
]} />;
export const Radar = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M19.07 4.93A10 10 0 0 0 6.99 3.34" }],
  ["path", { d: "M4 6h.01" }],
  ["path", { d: "M2.29 9.62A10 10 0 1 0 21.31 8.35" }],
  ["path", { d: "M16.24 7.76A6 6 0 1 0 8.23 16.67" }],
  ["path", { d: "M12 18h.01" }],
  ["path", { d: "M17.99 11.66A6 6 0 0 1 15.77 16.67" }],
  ["circle", { cx: "12", cy: "12", r: "2" }],
  ["path", { d: "m13.41 10.59 5.66-5.66" }]
]} />;
export const RotateCcw = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" }],
  ["path", { d: "M3 3v5h5" }]
]} />;
export const Search = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "m21 21-4.34-4.34" }],
  ["circle", { cx: "11", cy: "11", r: "8" }]
]} />;
export const Settings = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915",
      key: "1i5ecw"
    }
  ],
  ["circle", { cx: "12", cy: "12", r: "3" }]
]} />;
export const Settings2 = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M14 17H5" }],
  ["path", { d: "M19 7h-9" }],
  ["circle", { cx: "17", cy: "17", r: "3" }],
  ["circle", { cx: "7", cy: "7", r: "3" }]
]} />;
export const Square = (props: IconProps) => <Icon {...props} nodes={[
  ["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }]
]} />;
export const SquarePen = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" }],
  [
    "path",
    {
      d: "M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z",
      key: "ohrbg2"
    }
  ]
]} />;
export const X = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M18 6 6 18" }],
  ["path", { d: "m6 6 12 12" }]
]} />;

export const ArrowLeft = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "m12 19-7-7 7-7" }],
  ["path", { d: "M19 12H5" }]
]} />;

export const Plus = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M5 12h14" }],
  ["path", { d: "M12 5v14" }]
]} />;

export const LayoutGrid = (props: IconProps) => <Icon {...props} nodes={[
  ["rect", { width: "7", height: "7", x: "3", y: "3", rx: "1" }],
  ["rect", { width: "7", height: "7", x: "14", y: "3", rx: "1" }],
  ["rect", { width: "7", height: "7", x: "14", y: "14", rx: "1" }],
  ["rect", { width: "7", height: "7", x: "3", y: "14", rx: "1" }]
]} />;

export const GitBranch = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M15 6a9 9 0 0 0-9 9V3" }],
  ["circle", { cx: "18", cy: "6", r: "3" }],
  ["circle", { cx: "6", cy: "18", r: "3" }]
]} />;

export const MessageCircle = (props: IconProps) => <Icon {...props} nodes={[
  [
    "path",
    {
      d: "M2.992 16.342a2 2 0 0 1 .094 1.167l-1.065 3.29a1 1 0 0 0 1.236 1.168l3.413-.998a2 2 0 0 1 1.099.092 10 10 0 1 0-4.777-4.719"
    }
  ]
]} />;

export const UserRound = (props: IconProps) => <Icon {...props} nodes={[
  ["circle", { cx: "12", cy: "8", r: "5" }],
  ["path", { d: "M20 21a8 8 0 0 0-16 0" }]
]} />;

export const ArrowUpRight = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M7 7h10v10" }],
  ["path", { d: "M7 17 17 7" }]
]} />;

export const CircleAlert = (props: IconProps) => <Icon {...props} nodes={[
  ["circle", { cx: "12", cy: "12", r: "10" }],
  ["line", { x1: "12", x2: "12", y1: "8", y2: "12" }],
  ["line", { x1: "12", x2: "12.01", y1: "16", y2: "16" }]
]} />;

export const Funnel = (props: IconProps) => <Icon {...props} nodes={[["path", { d: "M10 20a1 1 0 0 0 .553.895l2 1A1 1 0 0 0 14 21v-7a2 2 0 0 1 .517-1.341L21.74 4.67A1 1 0 0 0 21 3H3a1 1 0 0 0-.742 1.67l7.225 7.989A2 2 0 0 1 10 14z" }]]} />;

export const Pin = (props: IconProps) => <Icon {...props} nodes={[
  ["path", { d: "M16 9V4l1-1H7l1 1v5l-3 3v3h6v6l1 1 1-1v-6h6v-3z" }]
]} />;
