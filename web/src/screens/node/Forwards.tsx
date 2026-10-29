import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { Empty } from "../../components/ui/extras";
import { fetcher } from "../../lib/api";
import { ago } from "../../lib/format";

interface Fwd { id: string; outbound_amount_msat?: number; fee_earned_msat?: number; forwarded_at: number }

export function Forwards() {
  const { data } = useSWR<{ items: Fwd[]; fees_earned_msat: number }>("/api/forwards", fetcher);
  return (
    <Card>
      <CardHeader>
        <CardTitle>Forwarded payments</CardTitle>
        {data && <p className="text-sm text-muted">Fees earned <Amount msat={data.fees_earned_msat} /></p>}
      </CardHeader>
      <CardContent className="divide-y divide-line p-0">
        {data?.items.length ? (
          data.items.map((f) => (
            <div key={f.id} className="flex justify-between px-5 py-3 text-sm">
              <span className="text-muted">{ago(f.forwarded_at)}</span>
              <span>Forwarded <Amount msat={f.outbound_amount_msat} /> · fee <Amount msat={f.fee_earned_msat} /></span>
            </div>
          ))
        ) : (
          <div className="p-4"><Empty>No forwards yet. Public channels earn fees by routing payments.</Empty></div>
        )}
      </CardContent>
    </Card>
  );
}
