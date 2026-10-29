import { Badge } from "../components/ui/badge";

export function Placeholder({ title, milestone }: { title: string; milestone: string }) {
  return (
    <section className="space-y-3">
      <h1 className="text-3xl font-semibold">{title}</h1>
      <Badge>Arrives in {milestone}</Badge>
    </section>
  );
}
