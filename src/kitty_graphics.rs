//! Kitty 图像协议：图像仓库、放置、删除与协议应答。
//!
//! 协议的结构部分——控制数据解析、`m=1` 分块重组、base64 解码、原始格式长度
//! 校验以及 PNG IHDR 嗅探——住在 [`jterm_core::kitty_graphics`]（anvil/2/3/4
//! 共用一份）。本模块只保留必须依赖解码器或终端状态的东西：图像仓库、放置、
//! 删除，以及把 `jterm_core` 的类型化错误翻译成线上应答的 responder。
//!
//! 内存预算取 [`Caps::SCREEN`]（64 MiB 编码 / 64 MiB 解码 / 16384 像素边长 /
//! 16 KiB 控制段 / 64 MiB 全部在途分块）。

use jterm_core::kitty_graphics as protocol;
use jterm_core::kitty_graphics::{Action, Assembled, Assembler, Caps, Command, Format, Step};
use std::collections::HashMap;

/// 本终端使用的协议预算：屏幕型终端（一张图可能铺满整个窗口）。
const CAPS: Caps = Caps::SCREEN;

const MAX_KITTY_IMAGES: usize = 100;
const MAX_KITTY_CACHE_MB: u64 = 256;
const MAX_KITTY_PLACEMENTS: usize = 1024;
/// 同时在途的分块 `a=T` 上传数量上限。核心按字节给在途分块封顶，一个只发空
/// 分块的客户端仍能开出任意多个槽位，所以这里另外给放置控制记账封顶。
const MAX_CHUNKED_PLACEMENTS: usize = 64;
/// 未被 PTY 取走的应答上限；超过后丢弃新应答而不是无界增长。
const MAX_PENDING_RESPONSE_BYTES: usize = 64 * 1024;
/// 应答里回显的错误文本长度上限。
const MAX_RESPONSE_MESSAGE_CHARS: usize = 160;

/// Kitty 图像。`data` 始终是解码后的 RGBA8。
#[derive(Debug, Clone)]
pub struct KittyImage {
    /// Monotonic content version used by the renderer cache. Re-transmitting an
    /// id must invalidate a same-sized GPU texture too.
    pub generation: u64,
    #[allow(dead_code)]
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// 一次图像放置。
///
/// 屏幕位置来自命令抵达时的光标格（`col`/`row`）——协议里 `x=`/`y=` **不是**
/// 屏幕坐标，而是裁剪源图的像素偏移，见 `src_x`/`src_y`。
#[derive(Debug, Clone)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    /// 左上角所在的屏幕列（命令抵达时的光标列）。
    pub col: u32,
    /// 左上角所在的屏幕行（命令抵达时的光标行）。
    pub row: u32,
    /// 占用的单元格列数（`c=`）。
    pub cols: u32,
    /// 占用的单元格行数（`r=`）。
    pub rows: u32,
    /// `x=`：源图裁剪起点的像素列。
    pub src_x: u32,
    /// `y=`：源图裁剪起点的像素行。
    pub src_y: u32,
    /// `w=`：源图裁剪宽度；`None`（含 `w=0`）表示一直到右边缘。
    pub src_width: Option<u32>,
    /// `h=`：源图裁剪高度；`None`（含 `h=0`）表示一直到下边缘。
    pub src_height: Option<u32>,
    pub z_index: i32,
}

/// 源图裁剪矩形，已对图像尺寸做过夹取。
pub type Crop = (u32, u32, u32, u32);

/// 把一次放置的 `x=`/`y=`/`w=`/`h=` 夹取到图像范围内。
///
/// 返回 `None` 表示裁剪矩形整体落在图像之外——这样的放置没有任何像素可画。
pub fn placement_crop(image: &KittyImage, placement: &KittyPlacement) -> Option<Crop> {
    if placement.src_x >= image.width || placement.src_y >= image.height {
        return None;
    }
    let available_width = image.width - placement.src_x;
    let available_height = image.height - placement.src_y;
    let width = placement
        .src_width
        .unwrap_or(available_width)
        .min(available_width);
    let height = placement
        .src_height
        .unwrap_or(available_height)
        .min(available_height);
    (width > 0 && height > 0).then_some((placement.src_x, placement.src_y, width, height))
}

/// 从图像的 RGBA 缓冲里复制出裁剪矩形。整图裁剪走克隆快路径。
pub fn crop_rgba(image: &KittyImage, crop: Crop) -> Vec<u8> {
    let (x, y, width, height) = crop;
    if x == 0 && y == 0 && width == image.width && height == image.height {
        return image.data.clone();
    }
    let stride = image.width as usize * 4;
    let row_bytes = width as usize * 4;
    let mut out = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = (y as usize + row) * stride + x as usize * 4;
        let end = start + row_bytes;
        if end <= image.data.len() {
            out.extend_from_slice(&image.data[start..end]);
        }
    }
    out
}

/// 一条命令的失败：上报给客户端的 kitty 错误码 + 人类可读文本。
#[derive(Debug)]
struct Failure {
    code: &'static str,
    message: String,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("EINVAL", message)
    }

    fn missing(message: impl Into<String>) -> Self {
        Self::new("ENOENT", message)
    }
}

impl From<protocol::Error> for Failure {
    /// `jterm_core` 只给结构化错误；线上错误码是 responder 的策略，由这里决定。
    fn from(error: protocol::Error) -> Self {
        let code = match error {
            protocol::Error::NotSupported(_) => "ENOTSUP",
            protocol::Error::Invalid(_) | protocol::Error::TooLarge => "EINVAL",
        };
        Self::new(code, error.to_string())
    }
}

/// 一条命令处理完之后要不要应答、以及应答给谁。
#[derive(Debug)]
enum Reply {
    /// 无需应答：分块还没结束，或者这个负载根本不是图像命令。
    Silent,
    /// 命令成功，按这些控制字段回 `OK`。
    Ok(ResponseTarget),
}

/// 应答的收件人：`i=` / `I=` / `p=` / `q=`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ResponseTarget {
    image_id: Option<u32>,
    image_number: Option<u32>,
    placement_id: Option<u32>,
    quiet: u8,
    quiet_specified: bool,
}

impl ResponseTarget {
    fn from_command(command: &Command<'_>) -> Self {
        Self {
            image_id: command.id,
            image_number: command.number,
            placement_id: command.placement,
            quiet: command.quiet,
            quiet_specified: command.get("q").is_some(),
        }
    }

    fn from_assembled(assembled: &Assembled) -> Self {
        Self {
            image_id: assembled.id,
            image_number: assembled.number,
            placement_id: assembled.placement,
            quiet: assembled.quiet,
            quiet_specified: true,
        }
    }

    /// 从一段可能压根解析不了的负载里尽力捞出应答字段。
    ///
    /// 命令被拒绝时（超长、非 UTF-8、控制对缺 `=`）也要能回错误，否则用 `i=`
    /// 寻址的客户端只能等超时。扫描长度按核心的控制段上限封顶。
    fn recover(payload: &[u8]) -> Self {
        let rest = payload.strip_prefix(b"G").unwrap_or(payload);
        let end = rest
            .iter()
            .take(protocol::MAX_CONTROL_BYTES)
            .position(|byte| *byte == b';')
            .unwrap_or_else(|| rest.len().min(protocol::MAX_CONTROL_BYTES));
        let Ok(control) = std::str::from_utf8(&rest[..end]) else {
            return Self::default();
        };
        let mut target = Self::default();
        for pair in control.split(',') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "i" => target.image_id = value.parse().ok(),
                "I" => target.image_number = value.parse().ok(),
                "p" => target.placement_id = value.parse().ok(),
                "q" => {
                    if let Some(quiet) = value.parse::<u8>().ok().filter(|quiet| *quiet <= 2) {
                        target.quiet = quiet;
                        target.quiet_specified = true;
                    }
                }
                _ => {}
            }
        }
        target
    }

    /// 只有带 `i=` 或 `I=` 的命令才会被应答（kitty 的寻址规则）。
    fn addressed(&self) -> bool {
        self.image_id.is_some() || self.image_number.is_some()
    }

    fn fields(&self) -> Option<String> {
        if !self.addressed() {
            return None;
        }
        let mut fields = Vec::with_capacity(3);
        if let Some(id) = self.image_id {
            fields.push(format!("i={id}"));
        }
        if let Some(number) = self.image_number {
            fields.push(format!("I={number}"));
        }
        // p=0 是「无放置」的占位，回显它只会让客户端困惑。
        if let Some(placement) = self.placement_id.filter(|id| *id != 0) {
            fields.push(format!("p={placement}"));
        }
        Some(fields.join(","))
    }
}

/// `a=T` / `a=p` 带来的放置控制。核心不建模这些键，由本模块从控制对里读出。
#[derive(Debug, Clone, Copy)]
struct PlacementRequest {
    placement_id: Option<u32>,
    col: u32,
    row: u32,
    cols: u32,
    rows: u32,
    src_x: u32,
    src_y: u32,
    src_width: Option<u32>,
    src_height: Option<u32>,
    z: i32,
}

impl PlacementRequest {
    fn from_command(
        command: &Command<'_>,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<Self, Failure> {
        Ok(Self {
            placement_id: command.placement,
            // 屏幕位置来自光标，不是 x=/y=。
            col: cursor_col,
            row: cursor_row,
            cols: command.u32_value("c")?.unwrap_or(1).max(1),
            rows: command.u32_value("r")?.unwrap_or(1).max(1),
            src_x: command.u32_value("x")?.unwrap_or(0),
            src_y: command.u32_value("y")?.unwrap_or(0),
            // w=0 / h=0 在协议里就是「到边缘为止」。
            src_width: command.u32_value("w")?.filter(|width| *width != 0),
            src_height: command.u32_value("h")?.filter(|height| *height != 0),
            z: command.i32_value("z")?.unwrap_or(0),
        })
    }
}

/// Kitty 图像协议状态管理
pub struct KittyGraphicsState {
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    assembler: Assembler,
    /// 待发往 PTY 的协议应答。
    responses: Vec<u8>,
    /// 在途分块传输的 `a=T` 放置控制，按图像 id 记账。它们只出现在首块里，
    /// 核心不会携带，所以在这里记一份，等最后一块落地时消费。
    pending_placements: HashMap<u32, PlacementRequest>,
    /// 在途分块传输的收件人。续块只带 `m=`/`q=`，没有它就无法回复最后一块。
    chunked_target: Option<ResponseTarget>,
    next_placement_id: u32,
    next_generation: u64,
    total_decoded: u32,
    total_bytes_processed: u64,
    total_image_memory: u64,
    access_order: std::collections::VecDeque<u32>,
}

impl KittyGraphicsState {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            placements: Vec::new(),
            assembler: Assembler::new(CAPS),
            responses: Vec::new(),
            pending_placements: HashMap::new(),
            chunked_target: None,
            next_placement_id: 1,
            next_generation: 1,
            total_decoded: 0,
            total_bytes_processed: 0,
            total_image_memory: 0,
            access_order: std::collections::VecDeque::new(),
        }
    }

    /// 取走待发往 PTY 的协议应答。
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    fn enforce_image_limits(&mut self) {
        while self.images.len() > MAX_KITTY_IMAGES
            || self.total_image_memory > MAX_KITTY_CACHE_MB * 1024 * 1024
        {
            if let Some(oldest_id) = self.access_order.pop_front() {
                if let Some(img) = self.images.remove(&oldest_id) {
                    self.total_image_memory = self
                        .total_image_memory
                        .saturating_sub(img.data.len() as u64);
                    self.placements.retain(|p| p.image_id != oldest_id);
                }
            } else {
                break;
            }
        }
    }

    /// 处理一个 APC-G 负载（`ESC _` 与 `ESC \` 之间的字节），光标位于原点。
    #[cfg(test)]
    pub fn parse_graphics_payload(&mut self, payload: &[u8]) -> Result<(), String> {
        self.parse_graphics_payload_at(payload, 0, 0)
    }

    /// 处理一个 APC-G 负载；`cursor_col`/`cursor_row` 是命令抵达时的光标格，
    /// `a=T` 与 `a=p` 的放置以它为左上角。
    ///
    /// 失败时会（在客户端允许的前提下）排入一条错误应答，然后把同一段文本作为
    /// `Err` 返回给调用方记日志。
    pub fn parse_graphics_payload_at(
        &mut self,
        payload: &[u8],
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), String> {
        let result = match self.dispatch(payload, cursor_col, cursor_row) {
            Ok(Reply::Silent) => Ok(()),
            Ok(Reply::Ok(target)) => {
                self.answer(target, None);
                Ok(())
            }
            Err(failure) => {
                // 续块自己不带 i=，失败时得借在途传输的身份来回话。
                let target = self.recover_target(payload);
                self.answer(target, Some(&failure));
                Err(failure.message)
            }
        };
        // 没有任何在途传输了，首块记下的那份状态也就无人认领。
        if !self.assembler.has_pending() {
            self.pending_placements.clear();
            self.chunked_target = None;
        }
        result
    }

    fn dispatch(
        &mut self,
        payload: &[u8],
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<Reply, Failure> {
        if !protocol::is_graphics_payload(payload) {
            return Ok(Reply::Silent);
        }
        // 首块携带的放置控制和收件人核心都不保留，先自己读一遍。控制段很短，
        // 这次解析相对 base64 解码可以忽略不计；解析失败留给 feed 去报错。
        let mut direct_placement = None;
        if let Ok(command) = protocol::parse_command(payload, &CAPS) {
            if command.action.is_transmit() && !command.is_continuation() {
                let request = match command.action {
                    Action::Display => Some(PlacementRequest::from_command(
                        &command, cursor_col, cursor_row,
                    )?),
                    _ => None,
                };
                if command.more {
                    self.chunked_target = Some(ResponseTarget::from_command(&command));
                    if let (Some(request), Some(id)) = (request, command.id) {
                        if self.pending_placements.len() >= MAX_CHUNKED_PLACEMENTS
                            && !self.pending_placements.contains_key(&id)
                        {
                            return Err(Failure::invalid(
                                "too many concurrent chunked kitty transfers",
                            ));
                        }
                        self.pending_placements.insert(id, request);
                    }
                } else {
                    direct_placement = request;
                }
            }
        }

        match self.assembler.feed(payload)? {
            Step::NotOurs => Ok(Reply::Silent),
            Step::NeedMore => Ok(Reply::Silent),
            Step::Ready(assembled) => {
                let target = ResponseTarget::from_assembled(&assembled);
                self.store(assembled, direct_placement)?;
                Ok(Reply::Ok(target))
            }
            Step::Other {
                command,
                interrupted,
            } => {
                let target = ResponseTarget::from_command(&command);
                // 删除本来就可以打断传输；其它动作打断了就是客户端的错。
                if interrupted && command.action != Action::Delete {
                    return Err(Failure::invalid(
                        "chunked kitty transfer interrupted by another action",
                    ));
                }
                match command.action {
                    Action::Query => {
                        // t=f/t=t/t=s 在这里也要报不支持，否则 a=q 探测会误以为
                        // 本终端支持文件传输。
                        command.require_direct_transport()?;
                        Ok(Reply::Ok(target))
                    }
                    Action::Placement => {
                        self.handle_placement(&command, cursor_col, cursor_row)?;
                        Ok(Reply::Ok(target))
                    }
                    Action::Delete => {
                        self.handle_delete(&command)?;
                        Ok(Reply::Ok(target))
                    }
                    _ => Err(Failure::new("ENOTSUP", "unsupported kitty graphics action")),
                }
            }
        }
    }

    /// 把一次完成的传输解码成 RGBA 并入库。
    ///
    /// `direct` 是单块 `a=T` 当场读出的放置控制；分块传输的那份在首块就存进了
    /// `pending_placements`。
    fn store(
        &mut self,
        assembled: Assembled,
        direct: Option<PlacementRequest>,
    ) -> Result<(), Failure> {
        let image_id = assembled
            .id
            .ok_or_else(|| Failure::invalid("kitty transfer without an image id (i=)"))?;
        let format = assembled.format;
        let display = assembled.display;
        // 无论这次是不是 a=T 都取走记账，免得半途中断的传输把放置留给下一张同 id 的图。
        let placement = direct.or_else(|| self.pending_placements.remove(&image_id));
        let (data, width, height) = match format {
            // 核心已经按 IHDR 校验过尺寸，解码器不会被一个 100 字节的负载
            // 骗去分配一整块画布。
            Format::Png => Self::decode_png(assembled.bytes)?,
            Format::Rgb8 | Format::Rgba8 => assembled.into_rgba8()?,
        };

        let data_size = data.len() as u64;
        self.total_decoded += 1;
        self.total_bytes_processed += data_size;
        // Re-transmitting an existing id replaces the old image: drop its
        // memory and its stale access-order entry so the counter doesn't
        // drift (and later underflow in enforce_image_limits).
        if let Some(old) = self.images.get(&image_id) {
            self.total_image_memory = self
                .total_image_memory
                .saturating_sub(old.data.len() as u64);
            self.access_order.retain(|&id| id != image_id);
        }
        self.total_image_memory += data_size;
        self.access_order.push_back(image_id);

        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.images.insert(
            image_id,
            KittyImage {
                generation,
                format,
                width,
                height,
                data,
            },
        );

        self.enforce_image_limits();

        if display {
            if let Some(request) = placement {
                self.add_placement(image_id, request);
            }
        }

        log::info!(
            "[KITTY_GRAPHICS] Stored image {} ({}x{}) format: {:?} | Stats: {} images, {}MB total",
            image_id,
            width,
            height,
            format,
            self.images.len(),
            self.total_bytes_processed / 1_000_000
        );
        Ok(())
    }

    /// 解码 `f=100`（PNG），返回 (RGBA 数据, 宽, 高)。
    fn decode_png(data: Vec<u8>) -> Result<(Vec<u8>, u32, u32), Failure> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(data));
        // 核心已经确认过 PNG 签名，这里不再让解码器去猜格式。
        reader.set_format(image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(CAPS.max_dimension);
        limits.max_image_height = Some(CAPS.max_dimension);
        limits.max_alloc = Some(CAPS.max_decoded_bytes as u64);
        reader.limits(limits);
        let decoded = reader
            .decode()
            .map_err(|error| Failure::invalid(format!("failed to decode PNG: {error}")))?;
        let (width, height) = (decoded.width(), decoded.height());
        let rgba = decoded.to_rgba8();
        log::debug!(
            "[KITTY_GRAPHICS] Decoded PNG {}x{} -> RGBA {}B",
            width,
            height,
            rgba.len()
        );
        Ok((rgba.into_raw(), width, height))
    }

    /// 处理放置操作 (a=p)
    fn handle_placement(
        &mut self,
        command: &Command<'_>,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), Failure> {
        let image_id = command
            .id
            .ok_or_else(|| Failure::invalid("kitty a=p without an image id (i=)"))?;
        if !self.images.contains_key(&image_id) {
            return Err(Failure::missing(format!(
                "kitty image {image_id} does not exist"
            )));
        }
        let request = PlacementRequest::from_command(command, cursor_col, cursor_row)?;
        self.add_placement(image_id, request);
        Ok(())
    }

    fn add_placement(&mut self, image_id: u32, placement: PlacementRequest) {
        let placement_id = placement.placement_id.or_else(|| {
            let id = self.next_placement_id;
            self.next_placement_id += 1;
            Some(id)
        });

        self.placements.push(KittyPlacement {
            image_id,
            placement_id,
            col: placement.col,
            row: placement.row,
            cols: placement.cols,
            rows: placement.rows,
            src_x: placement.src_x,
            src_y: placement.src_y,
            src_width: placement.src_width,
            src_height: placement.src_height,
            z_index: placement.z,
        });

        if self.placements.len() > MAX_KITTY_PLACEMENTS {
            let excess = self.placements.len() - MAX_KITTY_PLACEMENTS;
            self.placements.drain(0..excess);
        }

        // 按 z-order 排序
        self.placements.sort_by_key(|p| p.z_index);

        log::info!(
            "[KITTY_GRAPHICS] Placed image {} at cell ({},{}) span {}x{} crop ({},{}) z={}",
            image_id,
            placement.col,
            placement.row,
            placement.cols,
            placement.rows,
            placement.src_x,
            placement.src_y,
            placement.z
        );
    }

    /// 处理删除操作 (a=d)
    fn handle_delete(&mut self, command: &Command<'_>) -> Result<(), Failure> {
        if let Some(image_id) = command.id {
            if let Some(img) = self.images.remove(&image_id) {
                self.total_image_memory = self
                    .total_image_memory
                    .saturating_sub(img.data.len() as u64);
            }
            self.placements.retain(|p| p.image_id != image_id);
            self.access_order.retain(|&id| id != image_id);
            log::info!("[KITTY_GRAPHICS] Deleted image {}", image_id);
        } else if let Some(placement_id) = command.placement {
            self.placements
                .retain(|p| p.placement_id != Some(placement_id));
            log::info!("[KITTY_GRAPHICS] Deleted placement {}", placement_id);
        } else {
            return Err(Failure::invalid(
                "kitty a=d without an image id (i=) or placement id (p=)",
            ));
        }

        Ok(())
    }

    /// 借在途传输的身份补齐一个续块的收件人。
    fn recover_target(&self, payload: &[u8]) -> ResponseTarget {
        let recovered = ResponseTarget::recover(payload);
        if recovered.addressed() {
            return recovered;
        }
        match self.chunked_target {
            Some(chunked) if recovered.quiet_specified => ResponseTarget {
                quiet: recovered.quiet,
                quiet_specified: true,
                ..chunked
            },
            Some(chunked) => chunked,
            None => recovered,
        }
    }

    /// 排入一条协议应答。只回复带 `i=`/`I=` 的命令；`q=1` 吃掉 `OK`，
    /// `q=2` 连错误一起吃掉。
    fn answer(&mut self, target: ResponseTarget, failure: Option<&Failure>) {
        let Some(fields) = target.fields() else {
            return;
        };
        let body = match failure {
            None if target.quiet >= 1 => return,
            None => "OK".to_string(),
            Some(_) if target.quiet >= 2 => return,
            Some(failure) => {
                let message: String = failure
                    .message
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(MAX_RESPONSE_MESSAGE_CHARS)
                    .collect();
                format!("{}:{message}", failure.code)
            }
        };
        let response = format!("\x1b_G{fields};{body}\x1b\\");
        if self.responses.len().saturating_add(response.len()) <= MAX_PENDING_RESPONSE_BYTES {
            self.responses.extend_from_slice(response.as_bytes());
        } else {
            log::warn!(
                "[KITTY_GRAPHICS] Dropping protocol response: pending buffer reached {} bytes",
                MAX_PENDING_RESPONSE_BYTES
            );
        }
    }

    /// 获取性能统计
    #[allow(dead_code)]
    pub fn get_stats(&self) -> (u32, u64, usize) {
        (
            self.total_decoded,
            self.total_bytes_processed,
            self.images.len(),
        )
    }

    /// 获取所有放置
    pub fn get_placements(&self) -> &[KittyPlacement] {
        &self.placements
    }

    /// 获取图像
    pub fn get_image(&self, id: u32) -> Option<&KittyImage> {
        self.images.get(&id)
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    pub fn image_memory_mb(&self) -> u64 {
        self.total_image_memory / 1_000_000
    }

    /// 丢弃所有在途分块传输与未取走的应答。
    ///
    /// 这里没有时钟：半截上传的兜底是终端复位（RIS）加上核心的在途字节总量
    /// 上限，而不是一个挂钟超时。
    pub fn reset_transfers(&mut self) {
        self.assembler.reset();
        self.pending_placements.clear();
        self.chunked_target = None;
        self.responses.clear();
    }

    /// 清除所有数据
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.reset_transfers();
        self.total_decoded = 0;
        self.total_bytes_processed = 0;
        self.total_image_memory = 0;
        self.access_order.clear();
    }
}

impl Default for KittyGraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn encode(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }

    /// `[1, 2, 3, 4]` split the way a real client chunks a transfer: at a
    /// base64 boundary, not at a byte boundary (`AQI=` + `AwQ=` would put
    /// padding in the middle of the stream).
    const CHUNK_HEAD: &str = "AQID";
    const CHUNK_TAIL: &str = "BA==";

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([9, 8, 7, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    fn responses(state: &mut KittyGraphicsState) -> String {
        String::from_utf8(state.take_responses()).expect("responses are UTF-8")
    }

    #[test]
    fn standard_rgba_transmit_and_display_creates_placement() {
        let mut state = KittyGraphicsState::new();
        let data = [1, 2, 3, 4];

        state
            .parse_graphics_payload_at(
                format!("Gf=32,s=1,v=1,a=T,i=1;{}", encode(&data)).as_bytes(),
                6,
                3,
            )
            .unwrap();

        let image = state.get_image(1).unwrap();
        assert_eq!(image.format, Format::Rgba8);
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.data, data);
        let placement = &state.get_placements()[0];
        assert_eq!(placement.image_id, 1);
        assert_eq!((placement.cols, placement.rows), (1, 1));
    }

    #[test]
    fn raw_rgb_is_expanded_to_rgba() {
        let mut state = KittyGraphicsState::new();
        let rgb = [10, 20, 30, 40, 50, 60];

        state
            .parse_graphics_payload(format!("Gf=24,s=2,v=1,a=t,i=2;{}", encode(&rgb)).as_bytes())
            .unwrap();

        assert_eq!(
            state.get_image(2).unwrap().data,
            [10, 20, 30, 255, 40, 50, 60, 255]
        );
    }

    #[test]
    fn raw_transfer_rejects_invalid_dimensions_and_lengths() {
        let mut state = KittyGraphicsState::new();
        let error = state
            .parse_graphics_payload(
                format!("Gf=32,s=0,v=1,a=t,i=3;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap_err();
        assert!(error.contains("non-zero"), "{error}");
        assert!(state.get_image(3).is_none());

        let error = state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=t,i=4;{}", encode(&[1, 2, 3])).as_bytes(),
            )
            .unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        assert!(state.get_image(4).is_none());
    }

    #[test]
    fn standard_continuation_inherits_identity_and_auto_placement() {
        let mut state = KittyGraphicsState::new();

        state
            .parse_graphics_payload_at(
                format!("Gf=32,s=1,v=1,a=T,i=9,m=1;{CHUNK_HEAD}").as_bytes(),
                2,
                4,
            )
            .unwrap();
        assert!(state.get_image(9).is_none());
        state
            .parse_graphics_payload(format!("Gm=0;{CHUNK_TAIL}").as_bytes())
            .unwrap();

        assert_eq!(state.get_image(9).unwrap().data, [1, 2, 3, 4]);
        assert_eq!(state.get_placements().len(), 1);
        let placement = &state.get_placements()[0];
        assert_eq!(placement.image_id, 9);
        // 首块的光标格决定屏幕位置，最后一块不再挪动它。
        assert_eq!((placement.col, placement.row), (2, 4));
    }

    #[test]
    fn failed_continuation_is_cleared_and_can_be_retried() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(format!("Gf=32,s=1,v=1,a=t,i=12,m=1;{CHUNK_HEAD}").as_bytes())
            .unwrap();

        assert!(state.parse_graphics_payload(b"Gm=0;%%%bad%%%").is_err());

        state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=t,i=12;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap();
        assert_eq!(state.get_image(12).unwrap().data, [1, 2, 3, 4]);
    }

    // ---- defect (a): x=/y= are a source crop, not a screen position --------

    #[test]
    fn x_and_y_crop_the_source_image_and_do_not_move_it_on_screen() {
        let mut state = KittyGraphicsState::new();
        // 2x2 RGBA: 每个像素用它的行列编码。
        let pixels = [
            0, 0, 0, 255, 1, 0, 0, 255, // row 0
            0, 1, 0, 255, 1, 1, 0, 255, // row 1
        ];
        state
            .parse_graphics_payload_at(
                format!("Gf=32,s=2,v=2,a=T,i=20,x=1,y=1;{}", encode(&pixels)).as_bytes(),
                7,
                5,
            )
            .unwrap();

        let placement = &state.get_placements()[0];
        // 屏幕位置来自光标，x=/y= 没有参与。
        assert_eq!((placement.col, placement.row), (7, 5));
        assert_eq!((placement.src_x, placement.src_y), (1, 1));

        let image = state.get_image(20).unwrap();
        let crop = placement_crop(image, placement).unwrap();
        assert_eq!(crop, (1, 1, 1, 1));
        assert_eq!(crop_rgba(image, crop), [1, 1, 0, 255]);
    }

    #[test]
    fn a_crop_outside_the_image_yields_no_pixels() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=T,i=21,x=4,y=0;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap();
        let image = state.get_image(21).unwrap();
        assert!(placement_crop(image, &state.get_placements()[0]).is_none());
    }

    #[test]
    fn w_and_h_clamp_to_the_image_and_zero_means_to_the_edge() {
        let mut state = KittyGraphicsState::new();
        let pixels = vec![7u8; 2 * 2 * 4];
        state
            .parse_graphics_payload(
                format!("Gf=32,s=2,v=2,a=T,i=22,w=0,h=99;{}", encode(&pixels)).as_bytes(),
            )
            .unwrap();
        let image = state.get_image(22).unwrap();
        let placement = &state.get_placements()[0];
        assert_eq!(placement.src_width, None);
        assert_eq!(placement.src_height, Some(99));
        assert_eq!(placement_crop(image, placement).unwrap(), (0, 0, 2, 2));
    }

    // ---- defect (b): t= is validated ---------------------------------------

    #[test]
    fn a_file_transport_is_reported_as_unsupported_not_silently_decoded() {
        let mut state = KittyGraphicsState::new();
        let error = state
            .parse_graphics_payload(
                format!("Gf=32,t=f,s=1,v=1,a=T,i=30;{}", encode(b"/tmp/image.png")).as_bytes(),
            )
            .unwrap_err();

        assert!(error.contains("transport"), "{error}");
        assert!(state.get_image(30).is_none());
        assert_eq!(
            responses(&mut state),
            "\x1b_Gi=30;ENOTSUP:unsupported kitty graphics transport\x1b\\"
        );
    }

    #[test]
    fn a_query_may_not_ask_for_a_shared_memory_transport() {
        let mut state = KittyGraphicsState::new();
        assert!(state
            .parse_graphics_payload(b"Ga=q,t=s,i=31,f=32,s=1,v=1;AAAA")
            .is_err());
        assert!(responses(&mut state).contains("ENOTSUP"));
    }

    // ---- defect (c): the responder ----------------------------------------

    #[test]
    fn an_addressed_command_is_answered_and_an_anonymous_one_is_not() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=t,i=40;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap();
        assert_eq!(responses(&mut state), "\x1b_Gi=40;OK\x1b\\");

        state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=t;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap_err();
        assert_eq!(responses(&mut state), "");
    }

    #[test]
    fn a_query_is_answered_ok_and_echoes_a_non_zero_placement() {
        let mut state = KittyGraphicsState::new();
        state.parse_graphics_payload(b"Ga=q,i=41,p=7").unwrap();
        assert_eq!(responses(&mut state), "\x1b_Gi=41,p=7;OK\x1b\\");

        state.parse_graphics_payload(b"Ga=q,I=42,p=0").unwrap();
        assert_eq!(responses(&mut state), "\x1b_GI=42;OK\x1b\\");
    }

    #[test]
    fn quiet_one_suppresses_ok_and_quiet_two_also_suppresses_errors() {
        let mut state = KittyGraphicsState::new();
        state.parse_graphics_payload(b"Ga=q,i=43,q=1").unwrap();
        assert_eq!(responses(&mut state), "");

        state.parse_graphics_payload(b"Ga=p,i=44,q=1").unwrap_err();
        assert_eq!(
            responses(&mut state),
            "\x1b_Gi=44;ENOENT:kitty image 44 does not exist\x1b\\"
        );

        state.parse_graphics_payload(b"Ga=p,i=45,q=2").unwrap_err();
        assert_eq!(responses(&mut state), "");
    }

    #[test]
    fn a_command_rejected_before_parsing_is_still_answered() {
        let mut state = KittyGraphicsState::new();
        // 控制段超长：核心在读控制对之前就拒绝，responder 仍要能回话。
        let payload = format!("Gi=46,{}", "x".repeat(protocol::MAX_CONTROL_BYTES));
        state
            .parse_graphics_payload(payload.as_bytes())
            .unwrap_err();
        assert!(responses(&mut state).starts_with("\x1b_Gi=46;EINVAL:"));
    }

    #[test]
    fn a_failing_final_chunk_is_answered_with_the_transfers_identity() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(format!("Gf=32,s=1,v=1,a=t,i=47,m=1;{CHUNK_HEAD}").as_bytes())
            .unwrap();
        // 首块只是被缓存，不应答。
        assert_eq!(responses(&mut state), "");

        state.parse_graphics_payload(b"Gm=0;%%%bad%%%").unwrap_err();
        // 最后一块没有 i=，身份从在途传输借来。
        assert!(responses(&mut state).starts_with("\x1b_Gi=47;EINVAL:"));
    }

    #[test]
    fn a_completed_chunked_transfer_is_answered_once() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(format!("Gf=32,s=1,v=1,a=t,i=48,m=1;{CHUNK_HEAD}").as_bytes())
            .unwrap();
        state
            .parse_graphics_payload(format!("Gm=0;{CHUNK_TAIL}").as_bytes())
            .unwrap();
        assert_eq!(responses(&mut state), "\x1b_Gi=48;OK\x1b\\");
    }

    // ---- standardizations core adopted -------------------------------------

    #[test]
    fn a_missing_format_now_means_rgba_not_png() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(
                format!("Gs=1,v=1,a=t,i=50;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap();
        assert_eq!(state.get_image(50).unwrap().format, Format::Rgba8);
    }

    #[test]
    fn non_standard_format_aliases_are_gone() {
        let mut state = KittyGraphicsState::new();
        for alias in ["png", "jpeg", "jpg", "webp", "rgb", "rgba"] {
            let payload = format!("Gf={alias},s=1,v=1,a=t,i=51;{}", encode(&[1, 2, 3, 4]));
            let error = state
                .parse_graphics_payload(payload.as_bytes())
                .unwrap_err();
            assert!(error.contains("format"), "{alias}: {error}");
        }
        assert!(state.get_image(51).is_none());
    }

    #[test]
    fn png_transfers_decode_through_the_shared_ihdr_sniff() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(
                format!("Gf=100,a=t,i=52;{}", encode(&png_bytes(3, 2))).as_bytes(),
            )
            .unwrap();
        let image = state.get_image(52).unwrap();
        assert_eq!((image.width, image.height), (3, 2));
        assert_eq!(image.data.len(), 3 * 2 * 4);

        let error = state
            .parse_graphics_payload(format!("Gf=100,a=t,i=53;{}", encode(b"not a PNG")).as_bytes())
            .unwrap_err();
        assert!(error.contains("PNG"), "{error}");
    }

    #[test]
    fn dimensions_up_to_the_shared_cap_are_accepted() {
        // 旧的 8192 上限已被核心的 16384 取代。
        let mut state = KittyGraphicsState::new();
        let error = state
            .parse_graphics_payload(b"Gf=32,s=9000,v=1,a=t,i=54;AAAA")
            .unwrap_err();
        // 9000 > 8192：不再因为边长被拒，而是因为字节数对不上。
        assert!(error.contains("does not match"), "{error}");

        let error = state
            .parse_graphics_payload(b"Gf=32,s=16385,v=1,a=t,i=55;AAAA")
            .unwrap_err();
        assert!(error.contains("exceeds the configured limits"), "{error}");
    }

    #[test]
    fn id_and_number_are_mutually_exclusive() {
        let mut state = KittyGraphicsState::new();
        let error = state
            .parse_graphics_payload(b"Gf=32,s=1,v=1,a=t,i=56,I=57;AQIDBA==")
            .unwrap_err();
        assert!(error.contains("mutually exclusive"), "{error}");
    }

    #[test]
    fn placements_stay_sorted_by_z_index() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=T,i=60,z=5;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap();
        state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=T,i=61,z=-1;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap();

        let placements = state.get_placements();
        assert_eq!(placements[0].z_index, -1);
        assert_eq!(placements[1].z_index, 5);
    }

    #[test]
    fn a_placement_for_a_missing_image_is_reported_instead_of_stored() {
        let mut state = KittyGraphicsState::new();
        let error = state
            .parse_graphics_payload(b"Ga=p,i=70,c=2,r=1")
            .unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
        assert!(state.get_placements().is_empty());
    }

    #[test]
    fn deleting_an_image_drops_its_placements_and_frees_its_memory() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(
                format!("Gf=32,s=1,v=1,a=T,i=71;{}", encode(&[1, 2, 3, 4])).as_bytes(),
            )
            .unwrap();
        assert_eq!(state.image_count(), 1);

        state.parse_graphics_payload(b"Ga=d,d=I,i=71").unwrap();
        assert_eq!(state.image_count(), 0);
        assert!(state.get_placements().is_empty());
    }

    #[test]
    fn a_terminal_reset_drops_an_unfinished_transfer_without_a_clock() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(format!("Gf=32,s=1,v=1,a=t,i=80,m=1;{CHUNK_HEAD}").as_bytes())
            .unwrap();

        state.reset_transfers();

        // 复位之后续块无处可去。
        let error = state
            .parse_graphics_payload(format!("Gm=0;{CHUNK_TAIL}").as_bytes())
            .unwrap_err();
        assert!(error.contains("without a transfer in progress"), "{error}");
        assert!(state.get_image(80).is_none());
    }

    #[test]
    fn the_response_buffer_is_bounded() {
        let mut state = KittyGraphicsState::new();
        for _ in 0..8000 {
            let _ = state.parse_graphics_payload(b"Ga=q,i=90");
        }
        assert!(state.take_responses().len() <= MAX_PENDING_RESPONSE_BYTES);
    }
}
