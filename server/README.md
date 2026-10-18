# filebrowser-server

Comparte carpetas del PC por HTTP, **solo lectura**, con un enlace secreto. Proyecto aparte del
gestor de archivos: es otro binario y el gestor no lleva nada de este código de red.

```
cargo build --release -p filebrowser-server       # desde la raíz del repo (con HTTPS, ~1,9 MB)
cargo build --release -p filebrowser-server --no-default-features   # sin TLS, ~0,6 MB
target/release/filebrowser-server ~/Documentos ~/Fotos
target/release/filebrowser-server --bind 0.0.0.0 --ttl 2h ~/Descargas   # red local, caduca en 2 h
target/release/filebrowser-server --bind 0.0.0.0 --tls ~/Descargas      # con HTTPS autofirmado (compara la huella)
```

Imprime un enlace como `http://192.168.1.20:41873/s/<token>/` y un QR. El token (128 bits) solo existe
en memoria mientras el programa corre; sin él, todo responde 404.

## Qué hace
- Listado de carpetas (HTML sin JavaScript), orden por nombre/tamaño/fecha, búsqueda por nombre.
- Descarga con `Range` (saltar en vídeo/audio, reanudar), `ETag` y 304.
- Descarga de una carpeta como ZIP en streaming (sin comprimir, ZIP64).
- Vista previa pequeña de imágenes ≤ 1 MiB en el listado (la decodifica el navegador).
- Muestra en el navegador solo imágenes, vídeo, audio, PDF y texto; HTML, SVG y demás se descargan siempre.

## Qué no hace
- No sube, borra ni modifica nada. No hay cuentas. HTTPS solo con `--tls` y certificado autofirmado
  (el navegador avisa; compara la huella SHA-256 impresa en la terminal).
- Para salir a internet, usa un túnel (WireGuard, Tailscale, Cloudflare Tunnel); no abras el puerto del router.

## Seguridad
- Cada ruta se decodifica una vez, se valida por componentes y se canonicaliza; si el resultado sale de la
  carpeta compartida se responde 404 (enlaces simbólicos incluidos). Los archivos y carpetas que empiezan
  por `.` no se sirven salvo con `--hidden`.
- Solo archivos regulares y carpetas. Una petición por conexión, solo GET/HEAD, cabeceras ≤ 8 KiB,
  10 s para recibirlas, límite de conexiones simultáneas (global y 8 por IP) y de intentos fallidos por IP.
- Cabeceras `nosniff`, `Content-Security-Policy` restrictiva, `Referrer-Policy: no-referrer`.
- Se niega a ejecutarse como root/administrador sin `--i-know`.
- Los archivos se abren con `O_NOFOLLOW` (Unix) para cerrar la carrera con enlaces simbólicos; un directorio
  intermedio cambiado en ese instante por alguien con acceso local sigue siendo una ventana mínima.
- Plazo total de 10 s para handshake TLS y cabeceras; las IPv6 se limitan por /64.
- Limitaciones: la lectura muy lenta con el enlace en la mano puede retener hilos (8 por IP); tras un NAT
  o túnel todos los clientes comparten IP y por tanto el bloqueo por intentos fallidos; un ZIP que supera
  los topes (200 000 entradas) se cierra válido pero incompleto.

## Desde el gestor
«Compartir esta carpeta…» (menú contextual de la carpeta y menú principal) lanza este programa, si está
junto al gestor o en el PATH, sobre la carpeta actual y lo muestra en el terminal integrado.

## Interfaz web (el propio gestor en WebAssembly)
`/s/<token>/app/` abre **la misma aplicación** del gestor compilada a wasm32 y dibujada en un canvas; no es una
reimplementación. Los archivos se leen del servidor por XHR (capa `src/vfs/`). `server/build-web.sh` la compila y
deja el resultado en `server/webui/`, y el servidor lo embebe al compilar (sin esos ficheros funciona igual, solo
con los listados HTML). Es de **solo lectura** salvo que se arranque con `--write` (tope de subida: `--max-upload`
en MiB); cada petición que modifica exige la cabecera `X-FB: 1` y un `Origin` igual al `Host`.

## Usuario y contraseña
`--user NOMBRE` exige, además del enlace secreto, usuario y contraseña (HTTP Basic; el navegador los pide y los
recuerda). La contraseña (mínimo 8 caracteres) se toma de la variable `FB_PASSWORD`, de `--password-file ARCHIVO` o se
pregunta al arrancar sin eco; nunca va en la línea de órdenes. Los intentos fallidos cuentan para el bloqueo por IP.
Sin `--tls` (o un túnel) la contraseña viaja sin cifrar: el programa lo avisa si escuchas fuera de `127.0.0.1`.
