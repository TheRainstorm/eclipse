//! Fill an offscreen texture with the same deterministic tiles as the CPU fixture.
use anyhow::{Context, Result};
use windows::{
    core::{s, PCSTR},
    Win32::Graphics::{
        Direct3D::{Fxc::D3DCompile, *},
        Direct3D11::*,
    },
};

const HLSL: &str = r#"
cbuffer Scene : register(b0) { uint frame; uint tile_size; uint2 reserved; };
float4 vertex(uint i : SV_VertexID) : SV_Position {
    float2 p = float2((i << 1) & 2, i & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}
float4 pixel(float4 pos : SV_Position) : SV_Target {
    uint2 tile = uint2(pos.xy) / tile_size;
    uint h = tile.y * 65537u + tile.x + frame * 835001u;
    h = (h ^ (h >> 16)) * 0x7feb352du;
    h = (h ^ (h >> 15)) * 0x846ca68bu;
    h ^= h >> 16;
    return float4(float3((h >> 16) & 255, (h >> 8) & 255, h & 255) / 255.0, 1.0);
}"#;

fn compile(entry: PCSTR, profile: PCSTR) -> Result<Vec<u8>> {
    let mut code = None;
    unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            PCSTR::null(),
            None,
            None,
            entry,
            profile,
            0,
            0,
            &mut code,
            None,
        )?;
        let code = code.context("missing fixture bytecode")?;
        Ok(
            std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize())
                .to_vec(),
        )
    }
}
pub struct Tiles {
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    params: ID3D11Buffer,
    size: u32,
}
impl Tiles {
    pub fn new(device: &ID3D11Device, size: u32) -> Result<Self> {
        let (mut vertex, mut pixel, mut params) = (None, None, None);
        unsafe {
            device.CreateVertexShader(
                &compile(s!("vertex"), s!("vs_5_0"))?,
                None,
                Some(&mut vertex),
            )?;
            device.CreatePixelShader(
                &compile(s!("pixel"), s!("ps_5_0"))?,
                None,
                Some(&mut pixel),
            )?;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 16,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut params),
            )?;
        }
        Ok(Self {
            vertex: vertex.context("vertex shader")?,
            pixel: pixel.context("pixel shader")?,
            params: params.context("scene parameters")?,
            size,
        })
    }
    pub fn draw(
        &self,
        context: &ID3D11DeviceContext,
        target: &ID3D11RenderTargetView,
        width: u32,
        height: u32,
        frame: u32,
    ) {
        let params = [frame, self.size, 0, 0];
        let viewport = D3D11_VIEWPORT {
            Width: width as f32,
            Height: height as f32,
            MaxDepth: 1.,
            ..Default::default()
        };
        unsafe {
            context.UpdateSubresource(&self.params, 0, None, params.as_ptr().cast(), 0, 0);
            context.RSSetViewports(Some(&[viewport]));
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShader(&self.pixel, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            context.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            context.Draw(3, 0);
            context.OMSetRenderTargets(None, None);
        }
    }
}
