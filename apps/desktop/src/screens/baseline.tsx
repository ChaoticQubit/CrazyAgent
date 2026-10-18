import type { BaselineDetail } from "../engine-client";

export function BaselineInspection({ detail, blocked, approve, revoke }: {
  detail: BaselineDetail;
  blocked: boolean;
  approve: () => Promise<void>;
  revoke: () => Promise<void>;
}) {
  const state = detail.revoked ? "Revoked" : detail.approved ? "Approved" : "Awaiting approval";
  return (
    <article className="inspection" aria-labelledby="baseline-heading" data-testid="baseline-detail">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Baseline inspection</p>
          <h2 id="baseline-heading">Verification contract</h2>
        </div>
        <span className="badge" data-testid="baseline-state">{state}</span>
      </div>
      <dl className="identity-grid">
        <div><dt>Baseline</dt><dd className="mono">{detail.id}</dd></div>
        <div><dt>Project</dt><dd>{detail.project_id}</dd></div>
        <div><dt>Origin</dt><dd>{detail.origin}</dd></div>
        <div><dt>Read state</dt><dd>{detail.fresh ? "Snapshot" : "Stale snapshot"}</dd></div>
      </dl>
      <section aria-labelledby="requirements-heading">
        <h3 id="requirements-heading">Requirements</h3>
        <pre data-testid="baseline-requirements">{detail.requirements}</pre>
      </section>
      <section aria-labelledby="checks-heading">
        <h3 id="checks-heading">Required checks</h3>
        <div className="check-list">
          {detail.checks.map((check) => (
            <article className="check" key={check.id}>
              <div className="check-heading"><h4>{check.id}</h4><span>{check.kind} · {check.minimum_risk}</span></div>
              <p className="muted">{check.encoding === "hex" ? "Binary content shown as hexadecimal" : "Check content"}</p>
              <pre>{check.content}</pre>
            </article>
          ))}
        </div>
      </section>
      <section aria-labelledby="rules-heading">
        <h3 id="rules-heading">Risk rules</h3>
        {detail.rules.length === 0 ? <p className="muted">No additional path rules.</p> : (
          <ul>{detail.rules.map((rule) => <li key={rule.path_prefix}>{rule.path_prefix}: {rule.minimum_risk}</li>)}</ul>
        )}
        <p className="muted">Retry limit: {detail.retry_limit}. {detail.current ? "Current baseline." : "Not the current baseline."}</p>
      </section>
      <footer className="decision-bar">
        <p id="approval-availability" className="muted">Decisions apply to this exact inspected baseline. Revocation preserves the approval history.</p>
        <div className="button-row">
          <button disabled={blocked || !detail.fresh || detail.approved || detail.revoked} onClick={() => void approve()} aria-describedby="approval-availability" data-testid="approve-baseline">Approve baseline</button>
          <button disabled={blocked || !detail.fresh || detail.revoked} onClick={() => void revoke()} aria-describedby="approval-availability" data-testid="revoke-baseline">Revoke baseline</button>
        </div>
      </footer>
    </article>
  );
}
