/**
 * Shows why a selected feature or dimension has its current value.
 * Formula dependencies come first, then the upstream features, contract clauses, and TopoRef lineage.
 */
import { useMemo } from "react";
import type { DesignContract } from "../../types/scene.ts";
import type { CanonicalGraph } from "../../types/graphIr.ts";
import { explainModel } from "./explainModel.ts";

export interface ExplainModelPanelProps {
  readonly graph: CanonicalGraph;
  readonly nodeId?: string | null;
  readonly parameterName?: string | null;
  readonly contract?: DesignContract | null;
}

export function ExplainModelPanel({
  graph,
  nodeId = null,
  parameterName = null,
  contract = null,
}: ExplainModelPanelProps) {
  const model = useMemo(
    () => explainModel({ graph, nodeId, parameterName, contract }),
    [contract, graph, nodeId, parameterName],
  );
  if (!model.nodeId && !model.parameterName) {
    return (
      <section className="panel-card explain-model-panel" aria-label="模型解释">
        <h3 className="panel-card-title">模型解释</h3>
        <p className="workbench-panel-empty">选择特征或尺寸后，可以查看公式、上游节点、契约条款和拓扑血缘。</p>
      </section>
    );
  }

  return (
    <section className="panel-card explain-model-panel" aria-label="模型解释">
      <header className="panel-card-header">
        <h3 className="panel-card-title">模型解释</h3>
        <span className="mini-badge">{model.nodeName ?? model.parameterName}</span>
      </header>
      <h4 className="explain-heading">参数公式链</h4>
      {model.formulaChain.length === 0 ? (
        <p className="workbench-panel-empty">这个选择没有绑定参数公式。</p>
      ) : (
        <ol className="explain-list" aria-label="参数公式链">
          {model.formulaChain.map((link) => (
            <li key={link.name}>
              <code>{link.name}</code>
              <span> = {link.formula ?? link.valueText}</span>
              {link.dependsOn.length > 0 ? <span className="mini-badge">依赖 {link.dependsOn.join(", ")}</span> : null}
            </li>
          ))}
        </ol>
      )}
      <h4 className="explain-heading">上游输入</h4>
      {model.upstreamNodes.length === 0 ? (
        <p className="workbench-panel-empty">没有上游特征。</p>
      ) : (
        <ul className="explain-list" aria-label="上游输入节点">
          {model.upstreamNodes.map((node) => (
            <li key={node.id}>{node.name} <code>{node.id}</code></li>
          ))}
        </ul>
      )}
      <h4 className="explain-heading">设计契约条款</h4>
      {model.contractClauses.length === 0 ? (
        <p className="workbench-panel-empty">未绑定设计契约。</p>
      ) : (
        <ul className="explain-list" aria-label="设计契约条款">
          {model.contractClauses.map((clause) => (
            <li key={`${clause.kind}-${clause.clauseId}`}>
              <span className="mini-badge">{clause.kind}</span> {clause.text}
            </li>
          ))}
        </ul>
      )}
      <h4 className="explain-heading">TopoRef 血缘</h4>
      {model.topoLineage.length === 0 ? (
        <p className="workbench-panel-empty">没有记录拓扑血缘。</p>
      ) : (
        <ul className="explain-list" aria-label="TopoRef 血缘">
          {model.topoLineage.map((entry) => (
            <li key={`${entry.role}-${entry.topoRef}`}>
              <span className="topo-chip">{entry.topoRef}</span>
              <span className="mini-badge">{entry.role}</span>
              {entry.detail ? <span>{entry.detail}</span> : null}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
