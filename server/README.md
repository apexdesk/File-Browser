# filebrowser-server

Comparte carpetas del PC por HTTP, **solo lectura** por defecto (`--write` para subir, mover y borrar), con un enlace secreto. Proyecto aparte del
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
- Listado de carpetas (HTML sin JavaScript), orden por nombre/tamaño/fecha, búsqueda por nombre. Páginas y
  mensajes de error en español o inglés según `Accept-Language`.
- Descarga con `Range` (saltar en vídeo/audio, reanudar), `ETag` y 304.
- Descarga de una carpeta como ZIP en streaming (sin comprimir, ZIP64). Los enlaces a archivos de dentro van con
  su contenido; lo que no entra (enlaces a carpetas o que salen, archivos o carpetas ilegibles, topes) se apunta en
  `OMITIDOS.txt` / `SKIPPED.txt` dentro del propio ZIP.
- Vista previa pequeña de imágenes ≤ 1 MiB en el listado (la decodifica el navegador).
- Muestra en el navegador solo imágenes, vídeo, audio, PDF y texto; HTML, SVG y demás se descargan siempre.

## Qué no hace
- Sin `--write` no sube, borra ni modifica nada. No hay cuentas (como mucho un usuario con `--user`). HTTPS solo con `--tls` y certificado autofirmado
  (el navegador avisa; compara la huella SHA-256 impresa en la terminal).
- Para salir a internet, usa un túnel (WireGuard, Tailscale, Cloudflare Tunnel); no abras el puerto del router.

## Detrás de un túnel o proxy (Cloudflare Tunnel, Caddy…)
- Las escrituras comprueban que `Origin` tenga el mismo host[:puerto] que `Host`, con `http` o `https`: un
  proxy que termina TLS funciona siempre que **conserve la cabecera `Host`** que mandó el navegador.
- Todo lo que llega por el túnel viene de la misma IP (la del proxy): sin más, todos los clientes comparten el
  tope de 8 conexiones por IP y el bloqueo por intentos fallidos. Con `--proxy`, las conexiones desde
  127.0.0.1/::1 no tienen tope por IP (queda `--max-conn`) y los bloqueos usan la última dirección de
  `X-Forwarded-For`. Esa cabecera solo se cree a conexiones del propio equipo: usa `--proxy` solo si el proxy
  corre en la misma máquina y la añade.

## Seguridad
- Cada ruta se decodifica una vez, se valida por componentes y se canonicaliza; si el resultado sale de la
  carpeta compartida se responde 404 (enlaces simbólicos incluidos). Los archivos y carpetas que empiezan
  por `.` no se sirven salvo con `--hidden`.
- Solo archivos regulares y carpetas. Una petición por conexión, solo GET/HEAD (PUT/POST con `--write`),
  cabeceras ≤ 8 KiB, límite de conexiones simultáneas (global y 8 por IP) y de intentos fallidos por IP.
  Con todos los huecos ocupados, la conexión sin autenticar más antigua cede el suyo.
- Cabeceras `nosniff`, `Content-Security-Policy` restrictiva, `Referrer-Policy: no-referrer`.
- Se niega a ejecutarse como root (Unix) o como administrador elevado (Windows) sin `--i-know`.
- Los archivos se abren sin seguir un enlace en el último componente (`O_NOFOLLOW` en Unix,
  `FILE_FLAG_OPEN_REPARSE_POINT` en Windows); un directorio intermedio cambiado en ese instante por alguien con
  acceso local sigue siendo una ventana mínima.
- Plazo total de 5 s para handshake TLS y cabeceras; las IPv6 se limitan por /64.
- Limitaciones: la lectura muy lenta con el enlace en la mano puede retener hilos (8 por IP); tras un NAT
  o túnel todos los clientes comparten IP y por tanto el bloqueo por intentos fallidos (con `--user`, quien
  ya había entrado sigue dentro gracias a su cookie de sesión; ver `--proxy`); un ZIP que supera los topes
  (200 000 entradas) se cierra válido pero incompleto; un listado de más de 20 000 entradas o 4 MiB de HTML se
  recorta (tras ordenar: se ven las primeras en el orden elegido).
- HTTPS (`--tls`): el certificado vale lo que dure `--ttl` más un día, o 397 días sin `--ttl` (se imprime la fecha
  y se avisa una semana antes; hay que reiniciar para generar otro). Con `--tls`, `--ttl` se limita a 396 días.

## Desde el gestor
«Compartir esta carpeta…» (menú contextual de la carpeta y menú principal) lanza este programa, si está
junto al gestor o en el PATH, sobre la carpeta actual y lo muestra en el terminal integrado.

## Interfaz web (el propio gestor en WebAssembly)
`/s/<token>/app/` abre **la misma aplicación** del gestor compilada a wasm32 y dibujada en un canvas; no es una
reimplementación. Los archivos se leen del servidor por XHR (capa `src/vfs/`). `server/build-web.sh` la compila y
deja el resultado en `server/webui/`, y el servidor lo embebe al compilar (sin esos ficheros funciona igual, solo
con los listados HTML, que nunca escriben: con `--write` y sin la interfaz web el servidor lo avisa al arrancar).
Es de **solo lectura** salvo que se arranque con `--write` (tope de subida: `--max-upload` en MiB, publicado en
`/api/info` para que el cliente avise antes de enviar); cada petición que modifica exige la cabecera `X-FB: 1` y un
`Origin` con el mismo host que el `Host`. Las subidas van en segundo plano desde el archivo del navegador (sin
cargarlo en memoria), con progreso y «Cancelar», y nunca pisan un archivo existente (`If-None-Match: *`; si el
nombre se ocupa entretanto se elige otro). `cp` y `mv` tampoco pisan nada (409). Los errores de escritura llevan
el motivo en JSON (`{"ok":false,"error":"hidden"}`: carpeta con ocultos, `too_big`, `unreadable`, `exists`…).

## Usuario y contraseña
`--user NOMBRE` exige, además del enlace secreto, usuario y contraseña (HTTP Basic; el navegador los pide y los
recuerda). La contraseña (mínimo 8 caracteres) se toma de la variable `FB_PASSWORD`, de `--password-file ARCHIVO` o se
pregunta al arrancar sin eco (Unix y consola de Windows); nunca va en la línea de órdenes, y `FB_PASSWORD` se borra
del entorno tras leerla. Tras 20 contraseñas fallidas en un minuto desde una IP (o 100 en total) la contraseña deja de
comprobarse durante un minuto: todo es 429, acierte o no. Solo sigue entrando el navegador que ya se había
autenticado, por la cookie de sesión aleatoria que recibió entonces (no sustituye a la contraseña).
Sin `--tls` (o un túnel) la contraseña viaja sin cifrar: el programa lo avisa si escuchas fuera de `127.0.0.1`.
