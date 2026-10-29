#!/usr/bin/env python3
"""Extract exact production selection/admission/copy methods; no Cargo or desktop.

Fixture storage substitutes compression/grid allocation and SmallVec containers.
Hand-authored plans are passed through real materialization and selection methods.
Parser, plan construction, application routing, providers and native UI are absent.
"""
import argparse, json, os, re, subprocess
from pathlib import Path
p=argparse.ArgumentParser(); p.add_argument('--source',type=Path); p.add_argument('--out',type=Path); args=p.parse_args()
root=Path(__file__).resolve().parents[1]
source=(args.source or root/'src/terminal.rs').read_text()
out=args.out or root/'evidence/current'; out.mkdir(parents=True,exist_ok=True)
def item(marker):
 start=source.index(marker)
 if marker=='pub struct RawRowId': return source[start:source.index(';',start)+1]
 opening=source.index('{',start); end=opening+1; depth=1
 while depth:
  depth+=(source[end]=='{')-(source[end]=='}'); end+=1
 return source[start:end]
def decl(marker,derive):
 return '#[derive('+derive+')]\n'+item(marker)+'\n'
code='#![allow(dead_code, clippy::iter_cloned_collect)]\nuse std::{collections::{BTreeSet,HashMap,VecDeque},sync::atomic::{AtomicU64,Ordering}};\n'
code+='mod chat_mode { #[derive(Clone,Copy,Debug,Default,PartialEq,Eq)] pub struct ChatRow { reserved: bool } }\nmod chat_projection { #[derive(Clone,Debug,PartialEq,Eq)] pub struct ChatPlanKey; }\n'
for typ in ['RawRowId','RawCellOrigin','ViewportCell','ProjectionSourceRevision','RawCellBoundary','FinishedOutputRange','SyntheticRowKey','ChromeBoundary']:
 code+=decl('pub struct '+typ, 'Clone,Copy,Debug,PartialEq,Eq,Hash,PartialOrd,Ord' if typ=='RawRowId' else 'Clone,Copy,Debug,PartialEq,Eq,Hash')
code+=item('impl RawRowId {')+'\n'
for typ in ['ProjectionMode','ProjectedRowKind','ChromeKind']:
 code+=decl('pub enum '+typ,'Clone,Copy,Debug,PartialEq,Eq,Hash')
for typ in ['RawSliceSource','RawSliceOrigin','RawSlice','RawRowPlacement','ResolvedCollapse','RowSource','OriginSpan']:
 code+=decl('struct '+typ+' {','Clone,Copy,Debug,PartialEq,Eq')
for typ in ['ProjectionPlanRow','ProjectionPlan','ProjectionPlanCacheKey']:
 code+=decl('struct '+typ+' {','Clone,Debug,PartialEq,Eq')
code=re.sub(r'SmallVec<\[([^;]+);\s*\d+\]>',r'Vec<\1>',code)
code+=decl('pub struct ProjectionKey','Clone,Debug,PartialEq,Eq,Hash')
code+=decl('pub struct ProjectedViewport','Clone,Debug')
code+=decl('struct ProjectedProvenance','Debug')
code+=item('struct MaterializedProjection {')+'\n'+item('impl OriginSpan {')+'\n'+item('impl ProjectedViewport {')+'\n'
for typ in ['SelectionMode']:
 code+=decl('pub enum '+typ,'Clone,Copy,Debug,PartialEq,Eq')
code+=decl('pub struct Selection {','Clone,Copy,Debug,PartialEq,Eq')
code+=decl('struct ProjectedSelection {','Clone,Debug,PartialEq,Eq')
code+=decl('enum ProjectedSelectionAnchor {','Clone,Copy,Debug,PartialEq,Eq')
code+=decl('struct ProjectedSelectionEndpoint {','Clone,Copy,Debug,PartialEq,Eq')
for marker in ['impl ProjectedSelectionAnchor {','impl ProjectedSelectionEndpoint {']:
 if marker in source: code+=item(marker)+'\n'
code+='impl ProjectionPlan {\n'
for method in ['raw_absolute_row','raw_cell_document_row','selection_point_for_anchor','row_is_raw','raw_row_document_row','summary_owning_raw_cell','summary_row','selection_line_endpoints']:
 if 'fn '+method+'(' in source: code+=item('fn '+method+'(')+'\n'
code+='}\n'+item('fn copy_projected_cells(')+'\n'
code+='''
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)] struct StyleFlags(bool);
impl StyleFlags { fn wide_continuation(self)->bool { self.0 } fn set_wide(&mut self,value:bool) { assert!(!value); } }
#[derive(Clone,Copy,Debug,PartialEq,Eq)] pub struct TerminalCell { character:char, flags:StyleFlags }
impl Default for TerminalCell { fn default()->Self { Self { character:' ',flags:StyleFlags(false) } } }
struct ScrollbackLine { cells:Vec<TerminalCell>, id:RawRowId }
impl ScrollbackLine {
 fn compress(cells:&[TerminalCell],_wrapped:bool)->Self { Self { cells:cells.to_vec(),id:RawRowId::fresh() } }
 fn decompress(&self)->Vec<TerminalCell> { self.cells.clone() }
 fn raw_row_id(&self)->RawRowId { self.id }
}
struct TerminalGrid { cells:Vec<Vec<TerminalCell>>, row_wrapped:Vec<bool>, identity_revision:u64 }
impl TerminalGrid { fn rows(&self)->usize { self.cells.len() } fn row_len(&self)->usize { self.cells[0].len() } }
impl std::ops::Index<usize> for TerminalGrid { type Output=[TerminalCell]; fn index(&self,i:usize)->&Self::Output { &self.cells[i] } }
struct TerminalState {
 scrollback:VecDeque<ScrollbackLine>,grid:TerminalGrid,selection:Option<Selection>,projected_selection:Option<ProjectedSelection>,
 projection_plan_cache:Option<(ProjectionPlanCacheKey,std::sync::Arc<ProjectionPlan>)>,
 history_revision:u64,use_alt_buffer:bool,next_zone_id:u64,command_zones:Vec<()>,finished_output_provenance:Vec<()>
}
thread_local! { static PROJECTION_MATERIALIZED_ROWS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)}; static PROJECTION_MATERIALIZED_SLICES:std::cell::Cell<usize>=const {std::cell::Cell::new(0)}; }
impl TerminalState {
 fn new(cols:usize,rows:usize)->Self { Self { scrollback:VecDeque::new(),grid:TerminalGrid {cells:vec![vec![TerminalCell::default();cols];rows],row_wrapped:vec![false;rows],identity_revision:1},selection:None,projected_selection:None,projection_plan_cache:None,history_revision:0,use_alt_buffer:false,next_zone_id:0,command_zones:vec![],finished_output_provenance:vec![] } }
 fn chat_key_is_current(&self,_:&chat_projection::ChatPlanKey)->bool { panic!("Chat outside fixture scope") }
 fn block_spacing_boundaries(&self,enabled:bool)->Vec<RawRowId> { assert!(!enabled); Vec::new() }
 fn projected_row_to_absolute(&self,_:&ProjectedViewport,_:usize)->usize { panic!("raw bypass outside fixture scope") }
'''
for method in ['projection_selection_revision','projection_plan_key_matches_current_source','new_projected_selection','projected_selection_point','projected_row_real_column_bounds','projected_selection_same_surface','reanchor_projected_selection','copy_projected_selection','materialize_projection_plan','materialize_projection_plan_with_margin','projection_indexes']:
 code+=item('fn '+method+'(')+'\n'
for method in ['clear_text_selection','select_line_in_projection','extend_line_selection_in_projection']:
 code+=item('pub fn '+method+'(')+'\n'
code+='}\n#[path = '+json.dumps(str(root/'src/terminal/line_selection_tests.rs'))+'] mod line_selection_tests;\n'
assert 'return self.copy_projected_selection();' in item('pub fn copy_selection(')
(out/'extracted.rs').write_text(code)
rustc=os.environ.get('RUSTC','rustc')
subprocess.run([rustc,'--edition=2021','--deny','warnings','--test',str(out/'extracted.rs'),'-o',str(out/'line-selection-tests')],check=True)
r=subprocess.run([str(out/'line-selection-tests'),'--nocapture'],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
print(r.stdout,end=''); (out/'test.log').write_text(r.stdout)
raise SystemExit(r.returncode)
