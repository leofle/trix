//! The builder's canvas: a thin wrapper over rataflow, the only file that
//! names it, so the widget can be swapped or vendored without touching the
//! builder. Nodes are boxes you can click, drag and lay out; what a node
//! says and where it sits is the builder's.

use rataflow::{Edge, Flow, Node, StepEdge, Sugiyama};
use ratatui::Frame;
use ratatui::layout::Rect;

/// A node as the canvas draws it.
pub struct Box {
    pub id: String,
    pub at: [i32; 2],
    pub label: String,
}

pub struct Canvas {
    flow: Flow<rataflow::TextContent, StepEdge>,
}

fn graph(boxes: &[Box], edges: &[(String, String)]) -> Flow<rataflow::TextContent, StepEdge> {
    let nodes = boxes
        .iter()
        .map(|b| {
            Node::from_text(
                b.id.as_str(),
                (b.at[0] as f64, b.at[1] as f64),
                b.label.as_str(),
            )
        })
        .collect();
    let known = |id: &String| boxes.iter().any(|b| b.id == *id);
    let edges: Vec<Edge<StepEdge>> = edges
        .iter()
        .filter(|(a, b)| known(a) && known(b))
        .enumerate()
        .map(|(i, (a, b))| Edge::new(format!("e{i}"), a.as_str(), b.as_str()))
        .collect();
    Flow::with_graph(nodes, edges).unwrap_or_default()
}

impl Canvas {
    pub fn new(boxes: &[Box], edges: &[(String, String)]) -> Canvas {
        Canvas {
            flow: graph(boxes, edges),
        }
    }

    /// Draw it again from the workflow: after any edit.
    pub fn set(&mut self, boxes: &[Box], edges: &[(String, String)], selected: Option<&str>) {
        self.flow = graph(boxes, edges);
        if let Some(id) = selected {
            self.flow.select_node(id);
        }
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        f.render_widget(&mut self.flow, area);
    }

    /// A mouse event on the canvas: a click selects, a drag moves.
    pub fn mouse(&mut self, ev: crossterm::event::MouseEvent) {
        let _ = self.flow.handle_mouse_event(ev);
    }

    /// The node that's selected on the canvas, if one is.
    pub fn selected(&self) -> Option<String> {
        self.flow.selected_nodes().next().map(|n| n.id.clone())
    }

    /// Where each node sits now: after a drag, the builder keeps it.
    pub fn positions(&self) -> Vec<(String, [i32; 2])> {
        self.flow
            .nodes()
            .map(|n| {
                (
                    n.id.clone(),
                    [n.position.x.round() as i32, n.position.y.round() as i32],
                )
            })
            .collect()
    }

    /// Where an automatic layout would put each node: layered, top to bottom.
    pub fn layout(boxes: &[Box], edges: &[(String, String)]) -> Vec<(String, [i32; 2])> {
        let pairs: Vec<(&str, &str)> = edges
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let Ok(flow) =
            Flow::<rataflow::TextContent, StepEdge>::from_edges(&pairs, Sugiyama::vertical())
        else {
            return Vec::new();
        };
        let mut out: Vec<(String, [i32; 2])> = flow
            .nodes()
            .map(|n| {
                (
                    n.id.clone(),
                    [n.position.x.round() as i32, n.position.y.round() as i32],
                )
            })
            .collect();
        // A node no edge touches isn't in that graph: it keeps its row, to the right.
        let right = out.iter().map(|(_, p)| p[0]).max().unwrap_or(0) + 24;
        let loose: Vec<String> = boxes
            .iter()
            .filter(|b| !out.iter().any(|(id, _)| *id == b.id))
            .map(|b| b.id.clone())
            .collect();
        for (i, id) in loose.into_iter().enumerate() {
            out.push((id, [right, 1 + 6 * i as i32]));
        }
        out
    }
}
