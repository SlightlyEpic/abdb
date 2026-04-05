use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::StreamExt;

use crate::{
    accessor::Accessor,
    binder::{BoundExpr, BoundExprKind, BoundJoinCondition, BoundJoinKind, FunctionKind},
    common::{aliases::RecordId, txn::Txn},
    databox::{TupleLayout, Value},
    optimizer::*,
};

use super::{
    ExecError, ExecResult, Row,
    codec::{decode_row, encode_row},
    eval::{eval_expr, eval_to_bool},
};

pub struct Executor<A: Accessor> {
    accessor: Arc<A>,
    txn: Txn,
}

impl<A: Accessor> Executor<A> {
    pub fn new(accessor: Arc<A>, txn: Txn) -> Self {
        Self { accessor, txn }
    }

    pub async fn execute(&self, plan: PhysicalPlan) -> ExecResult<Vec<Row>> {
        self.exec(plan).await
    }

    async fn exec(&self, plan: PhysicalPlan) -> ExecResult<Vec<Row>> {
        match plan {
            PhysicalPlan::Nothing
            | PhysicalPlan::CreateTable(_)
            | PhysicalPlan::DropTable(_)
            | PhysicalPlan::AlterTable(_)
            | PhysicalPlan::CreateIndex(_)
            | PhysicalPlan::DropIndex(_) => Ok(vec![]),

            PhysicalPlan::Values(v) => self.exec_values(v),
            PhysicalPlan::SeqScan(s) => self.exec_seq_scan(s).await,
            PhysicalPlan::IndexScan(s) => self.exec_index_scan(s).await,
            PhysicalPlan::Filter(f) => self.exec_filter(f).await,
            PhysicalPlan::Projection(p) => self.exec_projection(p).await,
            PhysicalPlan::NestedLoopJoin(j) => self.exec_nl_join(j).await,
            PhysicalPlan::HashJoin(j) => self.exec_hash_join(j).await,
            PhysicalPlan::HashAggregate(a) => self.exec_hash_agg(a).await,
            PhysicalPlan::StreamAggregate(a) => {
                self.exec_hash_agg(PhysHashAggregate {
                    group_by: a.group_by,
                    aggregates: a.aggregates,
                    input: a.input,
                    schema: a.schema,
                })
                .await
            }
            PhysicalPlan::Sort(s) => self.exec_sort(s).await,
            PhysicalPlan::TopN(t) => self.exec_topn(t).await,
            PhysicalPlan::Limit(l) => self.exec_limit(l).await,
            PhysicalPlan::Distinct(d) => self.exec_distinct(*d.input).await,
            PhysicalPlan::HashDistinct(d) => self.exec_distinct(*d.input).await,
            PhysicalPlan::Insert(i) => self.exec_insert(i).await,
            PhysicalPlan::Update(u) => self.exec_update(u).await,
            PhysicalPlan::Delete(d) => self.exec_delete(d).await,
        }
    }

    // ---- Scans ----

    async fn exec_seq_scan(&self, scan: PhysSeqScan) -> ExecResult<Vec<Row>> {
        let layout = TupleLayout::from(scan.columns.clone());
        let stream = self.accessor.table_scan(self.txn, scan.table.oid).await?;
        let mut stream = std::pin::pin!(stream);
        let mut rows = Vec::new();
        while let Some(item) = stream.next().await {
            let (raw, _rid) = item?;
            let row = decode_row(&layout, &scan.columns, &raw);
            if scan.pushed_predicates.iter().all(|p| eval_to_bool(p, &row)) {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    async fn exec_index_scan(&self, scan: PhysIndexScan) -> ExecResult<Vec<Row>> {
        let layout = TupleLayout::from(scan.columns.clone());
        let start = scan
            .start_key
            .as_ref()
            .map(|e| eval_expr(e, &[]).to_bytes());
        let end = scan.end_key.as_ref().map(|e| eval_expr(e, &[]).to_bytes());
        let stream = self
            .accessor
            .index_scan(self.txn, scan.index.oid, start, end)
            .await?;
        let mut stream = std::pin::pin!(stream);
        let mut rows = Vec::new();
        while let Some(item) = stream.next().await {
            let (_key, rid) = item?;
            let raw = self
                .accessor
                .table_get(self.txn, scan.table.oid, rid)
                .await?;
            let row = decode_row(&layout, &scan.columns, &raw);
            if scan
                .residual_predicates
                .iter()
                .all(|p| eval_to_bool(p, &row))
            {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    // ---- Values ----

    fn exec_values(&self, values: PhysValues) -> ExecResult<Vec<Row>> {
        Ok(values
            .rows
            .iter()
            .map(|exprs| exprs.iter().map(|e| eval_expr(e, &[])).collect())
            .collect())
    }

    // ---- Filter ----

    async fn exec_filter(&self, f: PhysFilter) -> ExecResult<Vec<Row>> {
        let input = self.exec(*f.input).await?;
        Ok(input
            .into_iter()
            .filter(|row| eval_to_bool(&f.predicate, row))
            .collect())
    }

    // ---- Projection ----

    async fn exec_projection(&self, proj: PhysProjection) -> ExecResult<Vec<Row>> {
        let input_schema = proj.input.schema();
        let input = self.exec(*proj.input).await?;
        let mut rows = Vec::with_capacity(input.len());
        for row in &input {
            let out: Row = proj
                .exprs
                .iter()
                .zip(&proj.aliases)
                .map(|(expr, alias)| {
                    // Aggregate functions in projections above an aggregate node
                    // must be resolved by alias in the input schema.
                    if is_aggregate_expr(expr) {
                        input_schema
                            .columns
                            .iter()
                            .position(|c| c.name == *alias)
                            .and_then(|i| row.get(i).cloned())
                            .unwrap_or(Value::Null)
                    } else {
                        eval_expr(expr, row)
                    }
                })
                .collect();
            rows.push(out);
        }
        Ok(rows)
    }

    // ---- Nested Loop Join ----

    async fn exec_nl_join(&self, join: PhysNestedLoopJoin) -> ExecResult<Vec<Row>> {
        let left_rows = self.exec(*join.left).await?;
        let right_rows = self.exec(*join.right).await?;
        let lw = left_rows.first().map_or(0, |r| r.len());
        let rw = right_rows.first().map_or(0, |r| r.len());
        let mut result = Vec::new();
        let mut right_matched = vec![false; right_rows.len()];

        for lr in &left_rows {
            let mut found = false;
            for (ri, rr) in right_rows.iter().enumerate() {
                let combined = concat_rows(lr, rr);
                if eval_join_cond(&join.condition, &combined) {
                    result.push(combined);
                    found = true;
                    right_matched[ri] = true;
                }
            }
            if !found
                && matches!(
                    join.kind,
                    BoundJoinKind::LeftOuter | BoundJoinKind::FullOuter
                )
            {
                result.push(concat_rows(lr, &null_row(rw)));
            }
        }
        if matches!(
            join.kind,
            BoundJoinKind::RightOuter | BoundJoinKind::FullOuter
        ) {
            for (ri, rr) in right_rows.iter().enumerate() {
                if !right_matched[ri] {
                    result.push(concat_rows(&null_row(lw), rr));
                }
            }
        }
        Ok(result)
    }

    // ---- Hash Join ----

    async fn exec_hash_join(&self, join: PhysHashJoin) -> ExecResult<Vec<Row>> {
        let left_rows = self.exec(*join.left).await?;
        let right_rows = self.exec(*join.right).await?;
        let lw = left_rows.first().map_or(0, |r| r.len());
        let rw = right_rows.first().map_or(0, |r| r.len());

        // Build on right
        let mut table: HashMap<Vec<u8>, Vec<(usize, Row)>> = HashMap::new();
        for (idx, rr) in right_rows.iter().enumerate() {
            let key = eval_key(&join.right_keys, rr);
            table.entry(key).or_default().push((idx, rr.clone()));
        }

        let mut result = Vec::new();
        let mut right_matched = vec![false; right_rows.len()];

        // Probe with left
        for lr in &left_rows {
            let key = eval_key(&join.left_keys, lr);
            let mut found = false;
            if let Some(matches) = table.get(&key) {
                for (ri, rr) in matches {
                    let combined = concat_rows(lr, rr);
                    if let Some(ref res) = join.residual {
                        if !eval_to_bool(res, &combined) {
                            continue;
                        }
                    }
                    result.push(combined);
                    found = true;
                    right_matched[*ri] = true;
                }
            }
            if !found
                && matches!(
                    join.kind,
                    BoundJoinKind::LeftOuter | BoundJoinKind::FullOuter
                )
            {
                result.push(concat_rows(lr, &null_row(rw)));
            }
        }
        if matches!(
            join.kind,
            BoundJoinKind::RightOuter | BoundJoinKind::FullOuter
        ) {
            for (ri, rr) in right_rows.iter().enumerate() {
                if !right_matched[ri] {
                    result.push(concat_rows(&null_row(lw), rr));
                }
            }
        }
        Ok(result)
    }

    // ---- Hash Aggregate ----

    async fn exec_hash_agg(&self, agg: PhysHashAggregate) -> ExecResult<Vec<Row>> {
        let input = self.exec(*agg.input).await?;
        let mut groups: HashMap<Vec<u8>, (Row, Vec<Accumulator>)> = HashMap::new();

        for row in &input {
            let gvals: Row = agg.group_by.iter().map(|e| eval_expr(e, row)).collect();
            let key = row_to_key(&gvals);
            let entry = groups.entry(key).or_insert_with(|| {
                let accs = agg
                    .aggregates
                    .iter()
                    .map(|a| Accumulator::new(&a.kind, a.arg.is_some(), a.distinct))
                    .collect();
                (gvals.clone(), accs)
            });
            for (i, agg_expr) in agg.aggregates.iter().enumerate() {
                let val = agg_expr
                    .arg
                    .as_ref()
                    .map(|e| eval_expr(e, row))
                    .unwrap_or(Value::Null);
                entry.1[i].feed(&val);
            }
        }

        // Scalar aggregate on empty input: return one row with default aggregates
        if groups.is_empty() && agg.group_by.is_empty() {
            let accs: Vec<Accumulator> = agg
                .aggregates
                .iter()
                .map(|a| Accumulator::new(&a.kind, a.arg.is_some(), a.distinct))
                .collect();
            return Ok(vec![accs.iter().map(Accumulator::finish).collect()]);
        }

        Ok(groups
            .into_values()
            .map(|(mut gvals, accs)| {
                gvals.extend(accs.iter().map(Accumulator::finish));
                gvals
            })
            .collect())
    }

    // ---- Sort ----

    async fn exec_sort(&self, sort: PhysSort) -> ExecResult<Vec<Row>> {
        let mut rows = self.exec(*sort.input).await?;
        sort_rows(&mut rows, &sort.order_by);
        Ok(rows)
    }

    async fn exec_topn(&self, topn: PhysTopN) -> ExecResult<Vec<Row>> {
        let mut rows = self.exec(*topn.input).await?;
        sort_rows(&mut rows, &topn.order_by);
        let limit = eval_expr(&topn.limit, &[]).to_i64().unwrap_or(0) as usize;
        let offset = topn
            .offset
            .as_ref()
            .and_then(|e| eval_expr(e, &[]).to_i64())
            .unwrap_or(0) as usize;
        Ok(rows.into_iter().skip(offset).take(limit).collect())
    }

    // ---- Limit ----

    async fn exec_limit(&self, l: PhysLimit) -> ExecResult<Vec<Row>> {
        let rows = self.exec(*l.input).await?;
        let offset = l
            .offset
            .as_ref()
            .and_then(|e| eval_expr(e, &[]).to_i64())
            .unwrap_or(0) as usize;
        let iter = rows.into_iter().skip(offset);
        match l.limit.as_ref().and_then(|e| eval_expr(e, &[]).to_i64()) {
            Some(n) => Ok(iter.take(n as usize).collect()),
            None => Ok(iter.collect()),
        }
    }

    // ---- Distinct ----

    async fn exec_distinct(&self, input: PhysicalPlan) -> ExecResult<Vec<Row>> {
        let rows = self.exec(input).await?;
        let mut seen = HashSet::new();
        Ok(rows
            .into_iter()
            .filter(|row| seen.insert(row_to_key(row)))
            .collect())
    }

    // ---- DML: Insert ----

    async fn exec_insert(&self, ins: PhysInsert) -> ExecResult<Vec<Row>> {
        let PhysInsert {
            table,
            table_columns,
            target_columns,
            source,
            ..
        } = ins;
        let source_rows = self.exec(*source).await?;
        let layout = TupleLayout::from(table_columns.clone());
        let mut count: i64 = 0;

        for src_row in &source_rows {
            let mut full_row = vec![Value::Null; table_columns.len()];
            for (i, tc) in target_columns.iter().enumerate() {
                if let Some(pos) = table_columns.iter().position(|c| c.oid == tc.oid) {
                    if let Some(val) = src_row.get(i) {
                        full_row[pos] = val.clone();
                    }
                }
            }
            let tuple = encode_row(&layout, &table_columns, &full_row);
            self.accessor
                .table_insert(self.txn, table.oid, tuple)
                .await?;
            count += 1;
        }
        Ok(vec![vec![Value::I64(count)]])
    }

    // ---- DML: Update ----

    async fn exec_update(&self, upd: PhysUpdate) -> ExecResult<Vec<Row>> {
        let PhysUpdate {
            table,
            table_columns,
            assignments,
            input,
            ..
        } = upd;
        let scan_rows = self.scan_with_rids(*input).await?;
        let layout = TupleLayout::from(table_columns.clone());
        let mut count: i64 = 0;

        for (row, rid) in &scan_rows {
            let mut new_row = row.clone();
            for asn in &assignments {
                let val = eval_expr(&asn.value, row);
                if let Some(pos) = table_columns.iter().position(|c| c.oid == asn.column.oid) {
                    new_row[pos] = val;
                }
            }
            self.accessor
                .table_delete(self.txn, table.oid, *rid)
                .await?;
            let tuple = encode_row(&layout, &table_columns, &new_row);
            self.accessor
                .table_insert(self.txn, table.oid, tuple)
                .await?;
            count += 1;
        }
        Ok(vec![vec![Value::I64(count)]])
    }

    // ---- DML: Delete ----

    async fn exec_delete(&self, del: PhysDelete) -> ExecResult<Vec<Row>> {
        let PhysDelete { table, input, .. } = del;
        let scan_rows = self.scan_with_rids(*input).await?;
        let mut count: i64 = 0;
        for (_row, rid) in &scan_rows {
            self.accessor
                .table_delete(self.txn, table.oid, *rid)
                .await?;
            count += 1;
        }
        Ok(vec![vec![Value::I64(count)]])
    }

    // ---- Helpers ----

    /// Execute a scan-based plan returning rows paired with RecordIds.
    /// Used by UPDATE/DELETE which need RIDs to modify tuples in-place.
    async fn scan_with_rids(&self, plan: PhysicalPlan) -> ExecResult<Vec<(Row, RecordId)>> {
        match plan {
            PhysicalPlan::SeqScan(scan) => {
                let layout = TupleLayout::from(scan.columns.clone());
                let stream = self.accessor.table_scan(self.txn, scan.table.oid).await?;
                let mut stream = std::pin::pin!(stream);
                let mut rows = Vec::new();
                while let Some(item) = stream.next().await {
                    let (raw, rid) = item?;
                    let row = decode_row(&layout, &scan.columns, &raw);
                    if scan.pushed_predicates.iter().all(|p| eval_to_bool(p, &row)) {
                        rows.push((row, rid));
                    }
                }
                Ok(rows)
            }
            PhysicalPlan::IndexScan(scan) => {
                let layout = TupleLayout::from(scan.columns.clone());
                let start = scan
                    .start_key
                    .as_ref()
                    .map(|e| eval_expr(e, &[]).to_bytes());
                let end = scan.end_key.as_ref().map(|e| eval_expr(e, &[]).to_bytes());
                let stream = self
                    .accessor
                    .index_scan(self.txn, scan.index.oid, start, end)
                    .await?;
                let mut stream = std::pin::pin!(stream);
                let mut rows = Vec::new();
                while let Some(item) = stream.next().await {
                    let (_key, rid) = item?;
                    let raw = self
                        .accessor
                        .table_get(self.txn, scan.table.oid, rid)
                        .await?;
                    let row = decode_row(&layout, &scan.columns, &raw);
                    if scan
                        .residual_predicates
                        .iter()
                        .all(|p| eval_to_bool(p, &row))
                    {
                        rows.push((row, rid));
                    }
                }
                Ok(rows)
            }
            PhysicalPlan::Filter(f) => {
                let inner = self.scan_with_rids(*f.input).await?;
                Ok(inner
                    .into_iter()
                    .filter(|(row, _)| eval_to_bool(&f.predicate, row))
                    .collect())
            }
            _ => Err(ExecError::Internal(
                "DML input must be SeqScan, IndexScan, or Filter".into(),
            )),
        }
    }
}

// ======== Free functions ========

fn is_aggregate_expr(expr: &BoundExpr) -> bool {
    matches!(&expr.kind, BoundExprKind::Function(f) if f.kind.is_aggregate())
}

fn concat_rows(left: &[Value], right: &[Value]) -> Row {
    left.iter().chain(right.iter()).cloned().collect()
}

fn null_row(width: usize) -> Row {
    vec![Value::Null; width]
}

fn eval_key(exprs: &[BoundExpr], row: &[Value]) -> Vec<u8> {
    row_to_key(&exprs.iter().map(|e| eval_expr(e, row)).collect::<Vec<_>>())
}

fn eval_join_cond(cond: &BoundJoinCondition, row: &[Value]) -> bool {
    match cond {
        BoundJoinCondition::On(expr) => eval_to_bool(expr, row),
        BoundJoinCondition::Using(exprs) | BoundJoinCondition::Natural(exprs) => {
            exprs.iter().all(|e| eval_to_bool(e, row))
        }
        BoundJoinCondition::None => true,
    }
}

fn row_to_key(values: &[Value]) -> Vec<u8> {
    let mut key = Vec::new();
    for v in values {
        match v {
            Value::Null => key.push(0),
            other => {
                key.push(other.data_type().map(|d| d as u8 + 1).unwrap_or(0));
                let bytes = other.to_bytes();
                if matches!(other, Value::String(_)) {
                    key.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                }
                key.extend_from_slice(&bytes);
            }
        }
    }
    key
}

fn sort_rows(rows: &mut [Row], keys: &[PhysSortKey]) {
    rows.sort_by(|a, b| {
        for key in keys {
            let va = eval_expr(&key.expr, a);
            let vb = eval_expr(&key.expr, b);
            let nulls_first = key.nulls_first.unwrap_or(!key.asc);
            let ord = match (va.is_null(), vb.is_null()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => {
                    if nulls_first {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Greater
                    }
                }
                (false, true) => {
                    if nulls_first {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Less
                    }
                }
                (false, false) => va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal),
            };
            let ord = if key.asc { ord } else { ord.reverse() };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
}

// ======== Accumulator ========

struct Accumulator {
    kind: FunctionKind,
    has_arg: bool,
    distinct: bool,
    count: i64,
    sum: f64,
    min_val: Option<Value>,
    max_val: Option<Value>,
    seen: HashSet<Vec<u8>>,
}

impl Accumulator {
    fn new(kind: &FunctionKind, has_arg: bool, distinct: bool) -> Self {
        Self {
            kind: kind.clone(),
            has_arg,
            distinct,
            count: 0,
            sum: 0.0,
            min_val: None,
            max_val: None,
            seen: HashSet::new(),
        }
    }

    fn feed(&mut self, val: &Value) {
        if val.is_null() && self.kind != FunctionKind::Count {
            return;
        }
        if self.distinct && !val.is_null() && !self.seen.insert(val.to_bytes()) {
            return;
        }
        match self.kind {
            FunctionKind::Count => {
                if !self.has_arg || !val.is_null() {
                    self.count += 1;
                }
            }
            FunctionKind::Sum | FunctionKind::Avg => {
                if let Some(f) = val.to_f64() {
                    self.sum += f;
                    self.count += 1;
                }
            }
            FunctionKind::Min => {
                self.min_val = Some(match &self.min_val {
                    Some(cur) if val >= cur => cur.clone(),
                    _ => val.clone(),
                });
            }
            FunctionKind::Max => {
                self.max_val = Some(match &self.max_val {
                    Some(cur) if val <= cur => cur.clone(),
                    _ => val.clone(),
                });
            }
            _ => {}
        }
    }

    fn finish(&self) -> Value {
        match self.kind {
            FunctionKind::Count => Value::I64(self.count),
            FunctionKind::Sum => {
                if self.count == 0 {
                    Value::Null
                } else {
                    Value::F64(self.sum)
                }
            }
            FunctionKind::Avg => {
                if self.count == 0 {
                    Value::Null
                } else {
                    Value::F64(self.sum / self.count as f64)
                }
            }
            FunctionKind::Min => self.min_val.clone().unwrap_or(Value::Null),
            FunctionKind::Max => self.max_val.clone().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }
}
