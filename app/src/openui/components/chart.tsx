/** One chart component, drawn as plain SVG against the app's own tokens.
 * No chart library: three shapes is less code than a dependency, and a
 * generated dashboard must not be able to pull in a renderer we do not own. */
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { RenderProps } from "../context";

const SERIES = ["var(--primary)", "var(--status-active)", "var(--status-attention)", "var(--status-success)", "var(--status-danger)", "var(--muted-foreground)"];
const WIDTH = 320;
const HEIGHT = 140;

interface ChartProps { variant?: string; labels?: unknown; values?: unknown; title?: string }

export const Chart = (p: RenderProps<ChartProps>): JSX.Element => {
  const labels = () => (Array.isArray(p.props.labels) ? p.props.labels.map(value => (value == null ? "" : String(value))) : []);
  const values = () => (Array.isArray(p.props.values) ? p.props.values.map(toNumber) : []);
  const points = () => labels().map((label, index) => ({ label, value: values()[index] ?? 0 })).filter(point => Number.isFinite(point.value));
  const variant = () => (p.props.variant === "line" || p.props.variant === "pie" ? p.props.variant : "bar");
  const summary = () => points().map(point => `${point.label}: ${point.value}`).join(", ");
  return <figure data-slot="openui-chart" data-variant={variant()} class="flex min-w-0 flex-col gap-2">
    <Show when={p.props.title}><figcaption class="text-meta font-medium text-muted-foreground">{p.props.title}</figcaption></Show>
    <Show when={points().length > 0} fallback={<p class="text-meta text-muted-foreground">No data.</p>}>
      <svg role="img" aria-label={`${p.props.title ?? variant()} chart. ${summary()}`} viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        preserveAspectRatio="xMidYMid meet" class="h-auto w-full max-w-full overflow-visible">
        {variant() === "bar" ? bars(points()) : variant() === "line" ? line(points()) : pie(points())}
      </svg>
      <Show when={variant() === "pie"}>
        <ul class="flex flex-wrap gap-x-3 gap-y-1">
          <For each={points()}>{(point, index) => <li class="flex items-center gap-1 text-meta text-muted-foreground">
            <span aria-hidden="true" class="size-2 rounded-full" style={{ background: SERIES[index() % SERIES.length] }} />
            <span class="tabular-nums">{point.label} {point.value}</span>
          </li>}</For>
        </ul>
      </Show>
    </Show>
  </figure>;
};

interface Point { label: string; value: number }

function scale(points: Point[]): { top: number; bottom: number } {
  const values = points.map(point => point.value);
  return { top: Math.max(0, ...values) || 1, bottom: Math.min(0, ...values) };
}

function bars(points: Point[]): JSX.Element {
  const { top, bottom } = scale(points);
  const span = top - bottom || 1;
  const step = WIDTH / points.length;
  const zero = HEIGHT - 18 - ((0 - bottom) / span) * (HEIGHT - 26);
  return <g>
    <For each={points}>{(point, index) => {
      const height = (Math.abs(point.value) / span) * (HEIGHT - 26);
      const y = point.value >= 0 ? zero - height : zero;
      return <g>
        <rect x={index() * step + step * 0.18} y={y} width={step * 0.64} height={Math.max(height, 1)} rx="2" fill={SERIES[0]} />
        <text x={index() * step + step / 2} y={HEIGHT - 4} text-anchor="middle" font-size="10" fill="var(--muted-foreground)">{point.label}</text>
      </g>;
    }}</For>
    <line x1="0" y1={zero} x2={WIDTH} y2={zero} stroke="var(--border)" stroke-width="1" />
  </g>;
}

function line(points: Point[]): JSX.Element {
  const { top, bottom } = scale(points);
  const span = top - bottom || 1;
  const step = points.length > 1 ? WIDTH / (points.length - 1) : WIDTH;
  const at = (point: Point, index: number) => ({ x: points.length > 1 ? index * step : WIDTH / 2, y: HEIGHT - 18 - ((point.value - bottom) / span) * (HEIGHT - 26) });
  const path = points.map((point, index) => { const { x, y } = at(point, index); return `${index === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(1)}`; }).join(" ");
  return <g>
    <path d={path} fill="none" stroke={SERIES[0]} stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" />
    <For each={points}>{(point, index) => {
      const { x, y } = at(point, index());
      return <g><circle cx={x} cy={y} r="2" fill={SERIES[0]} /><text x={x} y={HEIGHT - 4} text-anchor="middle" font-size="10" fill="var(--muted-foreground)">{point.label}</text></g>;
    }}</For>
  </g>;
}

function pie(points: Point[]): JSX.Element {
  const total = points.reduce((sum, point) => sum + Math.max(point.value, 0), 0);
  if (total <= 0) return <g />;
  const radius = HEIGHT / 2 - 6;
  const centre = { x: WIDTH / 2, y: HEIGHT / 2 };
  let angle = -Math.PI / 2;
  const slices = points.map(point => {
    const sweep = (Math.max(point.value, 0) / total) * Math.PI * 2;
    const start = angle;
    angle += sweep;
    return { start, end: angle, sweep };
  });
  const edge = (a: number) => `${(centre.x + radius * Math.cos(a)).toFixed(2)} ${(centre.y + radius * Math.sin(a)).toFixed(2)}`;
  return <g><For each={slices}>{(slice, index) => slice.sweep <= 0 ? null : slice.sweep >= Math.PI * 2 - 1e-6
    ? <circle cx={centre.x} cy={centre.y} r={radius} fill={SERIES[index() % SERIES.length]} />
    : <path d={`M${centre.x} ${centre.y} L${edge(slice.start)} A${radius} ${radius} 0 ${slice.sweep > Math.PI ? 1 : 0} 1 ${edge(slice.end)} Z`}
        fill={SERIES[index() % SERIES.length]} stroke="var(--background)" stroke-width="1" />}</For></g>;
}

function toNumber(value: unknown): number {
  if (typeof value === "number") return value;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : Number.NaN;
}
