import { CurrencySettings } from "../../components/CurrencySettings";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { SettingsGroup } from "./SettingsLayout";

export function GeneralSettings() {
  return (
    <SettingsGroup title="General">
      <Card>
        <CardHeader>
          <CardTitle>Display currency</CardTitle>
          <CardDescription>Amounts are shown in sats, with their value in this currency next to them.</CardDescription>
        </CardHeader>
        <CardContent>
          <CurrencySettings />
        </CardContent>
      </Card>
    </SettingsGroup>
  );
}
