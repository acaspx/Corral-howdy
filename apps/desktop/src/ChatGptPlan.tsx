import { useEffect, useId, useRef, useState } from "react";
import { Alert, AlertDescription } from "@/components/glaze/ui/alert";
import { Button } from "@/components/glaze/ui/button";
import { Card, CardContent, CardFooter } from "@/components/glaze/ui/card";
import {
  Dialog,
  DialogAction,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/glaze/ui/dialog";
import { Status } from "@/components/glaze/ui/status";
import type { ChatGptStatus } from "./bindings";
import { chatgptApi, type ChatGptApi } from "./chatgptApi";
import { errorText } from "./keys";

/**
 * The ChatGPT plan (ADR-017, TP-SUB-05): sign in through the browser, see who is signed in and
 * that the plan is in use, manage its usage in ChatGPT, and sign out. The labels are the ones
 * OpenAI's interface guidelines ask for ("Continue with ChatGPT", "Using ChatGPT plan",
 * "Manage usage", the one-time welcome). Corral shows no OpenAI logo: the repo carries no
 * third-party marks.
 */
export function ChatGptPlan({
  api = chatgptApi,
  onChange,
}: {
  api?: ChatGptApi;
  /** Told after a sign-in or sign-out, so GPT's models in the agent editor follow (TP-SUB-08). */
  onChange?: () => void;
}) {
  const [status, setStatus] = useState<ChatGptStatus | null>(null);
  const [waiting, setWaiting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unconfirmed, setUnconfirmed] = useState(false);
  const cancelled = useRef(false);
  const headingId = useId();

  useEffect(() => {
    let live = true;
    api.status().then(
      (s) => live && setStatus(s),
      (e: unknown) => live && setError(`Couldn't read the ChatGPT sign-in: ${errorText(e)}`),
    );
    return () => {
      live = false;
    };
  }, [api]);

  async function signIn() {
    cancelled.current = false;
    setWaiting(true);
    setError(null);
    setUnconfirmed(false);
    try {
      setStatus(await api.signIn());
      onChange?.();
    } catch (e: unknown) {
      // Cancel is the user's own choice, so it isn't an error.
      if (!cancelled.current) setError(errorText(e));
    } finally {
      setWaiting(false);
    }
  }

  async function cancel() {
    cancelled.current = true;
    try {
      await api.cancel();
    } catch (e: unknown) {
      setError(`Couldn't cancel the sign-in: ${errorText(e)}`);
    }
  }

  async function signOut() {
    setBusy(true);
    setError(null);
    try {
      const out = await api.signOut();
      setStatus(out.status);
      setUnconfirmed(!out.revoked);
      onChange?.();
    } catch (e: unknown) {
      setError(`Couldn't sign out: ${errorText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function manageUsage() {
    try {
      await api.manageUsage();
    } catch (e: unknown) {
      setError(`Couldn't open ChatGPT's settings: ${errorText(e)}`);
    }
  }

  async function welcomed() {
    try {
      setStatus(await api.welcomed());
    } catch (e: unknown) {
      setError(errorText(e));
    }
  }

  const signedIn = status?.signedIn === true;

  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-3">
      <div className="flex flex-col gap-1">
        <h2 id={headingId} className="type-heading">
          ChatGPT plan
        </h2>
        <p className="type-body-sm text-text-2">
          Run GPT agents on your ChatGPT Plus or Pro plan instead of an OpenAI key. Every tool call
          still asks Corral first.
        </p>
      </div>
      {status && (
        <Card>
          {signedIn ? (
            <>
              <CardContent className="gap-1">
                <p className="type-label">Signed in as {status.email}</p>
                <Status status="ok" size="sm">
                  Using ChatGPT plan
                </Status>
              </CardContent>
              <CardFooter>
                <Button variant="secondary" size="sm" onClick={() => void manageUsage()}>
                  Manage usage
                </Button>
                <Button variant="ghost" size="sm" pending={busy} onClick={() => void signOut()}>
                  Sign out
                </Button>
              </CardFooter>
            </>
          ) : waiting ? (
            <>
              <CardContent>
                <Status status="running" size="sm">
                  Finish signing in in your browser
                </Status>
              </CardContent>
              <CardFooter>
                <Button variant="secondary" size="sm" onClick={() => void cancel()}>
                  Cancel
                </Button>
              </CardFooter>
            </>
          ) : (
            <>
              <CardContent>
                <p className="type-body-sm text-text-2">
                  Not signed in. ChatGPT opens in your browser, and Corral never sees your password.
                </p>
              </CardContent>
              <CardFooter>
                <Button size="sm" onClick={() => void signIn()}>
                  Continue with ChatGPT
                </Button>
              </CardFooter>
            </>
          )}
        </Card>
      )}
      {unconfirmed && (
        <Alert status="warn" role="status">
          <AlertDescription>
            Signed out on this Mac, but ChatGPT didn't confirm it. To be sure, disconnect Corral in
            ChatGPT's settings.
          </AlertDescription>
        </Alert>
      )}
      {error && (
        <Alert status="fail">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      <Dialog
        open={status?.showWelcome === true}
        onOpenChange={(open) => {
          if (!open) void welcomed();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>You're using your ChatGPT plan</DialogTitle>
            <DialogDescription>
              GPT agents in Corral now count against your plan's usage, not an OpenAI key. You can
              see and limit it with Manage usage.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <DialogAction close>Got it</DialogAction>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
