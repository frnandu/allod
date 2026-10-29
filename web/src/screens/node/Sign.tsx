import { useState } from "react";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton, Field } from "../../components/ui/extras";
import { Input, Textarea } from "../../components/ui/input";
import { api } from "../../lib/api";

export function Sign() {
  const [message, setMessage] = useState("");
  const [signature, setSignature] = useState<string | null>(null);
  const [v, setV] = useState({ message: "", signature: "", pubkey: "" });
  const [valid, setValid] = useState<boolean | null>(null);
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Card>
        <CardHeader><CardTitle>Sign with your node key</CardTitle></CardHeader>
        <CardContent className="space-y-4">
          <Field label="Message"><Textarea value={message} onChange={(e) => setMessage(e.target.value)} /></Field>
          <Button disabled={!message} onClick={async () => setSignature((await api.post<{ signature: string }>("/api/sign-message", { message })).signature)}>Sign</Button>
          {signature && (
            <div className="space-y-2">
              <p className="break-all rounded-lg bg-surface-2 p-2 font-mono text-xs">{signature}</p>
              <CopyButton value={signature} />
            </div>
          )}
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>Verify a signature</CardTitle></CardHeader>
        <CardContent className="space-y-4">
          <Field label="Message"><Textarea value={v.message} onChange={(e) => setV({ ...v, message: e.target.value })} /></Field>
          <Field label="Signature"><Input value={v.signature} onChange={(e) => setV({ ...v, signature: e.target.value })} /></Field>
          <Field label="Node ID"><Input value={v.pubkey} onChange={(e) => setV({ ...v, pubkey: e.target.value })} /></Field>
          <Button variant="secondary" onClick={async () => setValid((await api.post<{ valid: boolean }>("/api/verify-message", v)).valid)}>Verify</Button>
          {valid != null && <Alert tone={valid ? "info" : "danger"} title={valid ? "Valid signature" : "Invalid signature"} />}
        </CardContent>
      </Card>
    </div>
  );
}
