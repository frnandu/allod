import { CurrencySettings } from "../components/CurrencySettings";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";

export function Settings() {
  return (
    <div className="space-y-6">
      <h1 className="text-3xl font-semibold">Settings</h1>
      <Card>
        <CardHeader>
          <CardTitle>Display currency</CardTitle>
          <CardDescription>Amounts are shown in sats, with their value in this currency next to them.</CardDescription>
        </CardHeader>
        <CardContent>
          <CurrencySettings />
        </CardContent>
      </Card>
    </div>
  );
}
