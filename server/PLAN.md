# File Browser Server — planteamiento

Proyecto **aparte** dentro del mismo repo que `filebrowser`, en `server/`: crate propio
(*workspace member*) con su `Cargo.toml` y binario `filebrowser-server`. Los ejecutables clásicos
no se tocan en `src/`, no ganan dependencias de red y no cambian de tamaño.
El `Cargo.toml` raíz solo recibe `[workspace] members = ["server"]` y `default-members = ["."]`,
de modo que `cargo build --release` sigue compilando solo el gestor y el servidor se construye con
`cargo build --release -p filebrowser-server`.

## Objetivo
Servir carpetas del PC por HTTP para verlas y descargarlas desde un navegador (móvil u otro PC),
con el menor tamaño y la menor superficie de ataque posibles. Sin cuentas, sin base de datos,
sin panel de administración.

## No objetivos (v1)
- Subir, borrar, renombrar o editar archivos por defecto (solo lectura; la escritura es opt-in con `--write`, ver M4).
- Usuarios y permisos múltiples (hay un único usuario opcional, `--user`).
- Exposición directa a internet (se recomienda túnel: Tailscale, WireGuard, Cloudflare Tunnel).
- Streaming con transcodificación, WebDAV, SMB.

## Uso
```
filebrowser-server ~/Documentos ~/Fotos        # comparte esas carpetas
filebrowser-server --port 8080 --bind 0.0.0.0  # por defecto: 127.0.0.1 y puerto aleatorio
filebrowser-server --ttl 2h ~/Fotos            # caduca a las 2 h; solo lectura (modo por defecto)
```
Al arrancar imprime una URL con token: `http://192.168.1.20:41873/s/<token>/` y un QR en terminal.
Ctrl+C la invalida. El token no se guarda en disco.

## Seguridad (lo central)
1. **Token aleatorio** de 128 bits (de `getrandom`) en la ruta; sin token válido, 404 igual que una ruta inexistente.
   Comparación en tiempo constante. Limitador de intentos fallidos por IP.
2. **Confinamiento**: cada raíz se canonicaliza al inicio. Cada petición: decodificar % una sola vez,
   rechazar `..`, `\`, NUL, `:` en Windows, y tras unir comprobar que el `canonicalize` sigue
   dentro de la raíz (los enlaces simbólicos que salen se omiten). Apertura sin seguir el último
   componente simbólico cuando se pueda (`O_NOFOLLOW`).
3. **Solo ficheros regulares y directorios**: nada de dispositivos, FIFOs ni sockets.
4. **Límites**: máx. conexiones simultáneas, cabeceras ≤ 8 KiB, timeout de lectura/escritura,
   máx. entradas y bytes por listado, sin `Transfer-Encoding`, solo GET/HEAD (PUT/POST solo con `--write`).
5. **Cabeceras**: `Content-Security-Policy` estricta, `X-Content-Type-Options: nosniff`,
   `Content-Disposition: attachment` por defecto (previsualización inline solo de imágenes/vídeo/PDF
   con tipos MIME de una lista cerrada; SVG y HTML siempre como descarga).
6. **Escape de HTML** en nombres de archivo en el listado; nombres con saltos de línea o control, saneados.
7. **Red**: por defecto solo `127.0.0.1`. `--bind` explícito para LAN, con aviso si no se usa `--tls`
   (HTTPS con certificado propio generado con rustls y huella impresa en consola, ver M3).
8. **Sin ejecución**: ningún endpoint lanza procesos. Miniaturas solo con decodificadores acotados
   (los mismos topes de píxeles/memoria ya auditados en el gestor).
9. **Privilegios**: se niega a arrancar como root (Unix) o como administrador elevado (Windows, token con
   elevación) salvo `--i-know`.

## Funciones v1
- Listado de directorio (HTML sin JS obligatorio, ordenable, responsive).
- Descarga con `Range` (reanudar y saltar en vídeo/audio), `ETag`/`Last-Modified`.
- Descarga de carpeta como ZIP en streaming (sin buffer completo, con tope opcional).
- Previsualización inline: imágenes, vídeo, audio, PDF, texto.
- Miniaturas bajo demanda con caché en memoria acotada (fase 2).
- Búsqueda por nombre dentro de la raíz (fase 2).

## Arquitectura
- **Crate** `server/`, binario `filebrowser-server`. Servidor HTTP/1.1 síncrono con pool de hilos fijo
  (`tiny_http` o implementación mínima propia sobre `std::net`); sin tokio para mantener el tamaño.
- Módulos: `main` (CLI), `server` (aceptación, límites), `route` (token + rutas), `jail`
  (confinamiento, la parte más revisada), `index` (HTML del listado), `range`, `zip_stream`, `mime`.
- **Reutilización del gestor**: M0 no depende de la `lib` (que arrastra egui). Para miniaturas
  (M2) se extraen las piezas puras (decodificación acotada) a un crate pequeño compartido, o se
  usa la `lib` tras un *feature* sin GUI. Nunca al revés: `filebrowser` no depende de `filebrowser-server`.
- Registro mínimo por stderr (sin rutas completas del sistema de archivos en las respuestas de error).

## Integración opcional con el gestor (sin tocar su binario)
- Fase 3: una entrada de menú «Compartir esta carpeta…» que simplemente **lanza** `filebrowser-server`
  si está instalado (como cualquier otro programa externo), mostrando la URL. Si no está instalado,
  la entrada no aparece. El gestor no enlaza ningún código de red.

## Distribución
- Artefacto separado: `filebrowser-server-{linux,windows,macos}` (perfil de release propio del crate); objetivo < 2 MB (`opt-level="z"`, LTO, strip).
- Pruebas (`cargo test -p filebrowser-server`): unitarias de `jail` (traversal, simbólicos, codificaciones dobles,
  Unicode), del analizador HTTP, rangos, ZIP (leído con la crate `zip`) y HTML; de extremo a extremo en `e2e.rs` y
  `write.rs`, con el servidor real en 127.0.0.1 y puerto efímero dentro del proceso de pruebas (listado, descarga,
  rangos, 304, ZIP, búsqueda, token en todas las rutas, Basic y bloqueo, escritura, ocultos, árboles profundos,
  conexiones mudas y HTTPS con la huella fijada); y una prueba de humo del analizador con 30 000 mutaciones
  deterministas (semilla fija, sin dependencias) más 300 peticiones mutadas contra el servidor vivo.
  **No hay** fuzzing guiado por cobertura (cargo-fuzz) ni pruebas con `curl`; las pruebas propias de Windows
  (nombres 8.3, uniones, `MoveFileEx`) solo se comprueban compilando (`cargo xwin check`), no se ejecutan aquí.

## Estado y hitos
- **M0 (hecho)** crate `server/`, `jail` con tests (traversal, `%2e%2e`, doble codificación, enlaces que salen o apuntan a ocultos,
  prefijo parecido), token, listado, descarga con `Range`/`ETag`/304, solo `127.0.0.1` por defecto.
- **M1 (hecho)** `--bind` para LAN, límite de conexiones (503) global y por IP, tiempos de lectura (408) y escritura, cabeceras acotadas (431),
  bloqueo por IP tras 20 tokens falsos en un minuto (429), ZIP en streaming (stored + ZIP64, validado con `unzip` y la crate `zip`),
  QR en terminal, `--ttl`, negativa a ejecutarse como root, dotfiles ocultos por defecto.
- **M2 (hecho)** previsualización inline de lista cerrada (imágenes, vídeo, audio, PDF, texto), búsqueda por nombre
  (tope de 300 resultados, 3 s y 200 000 entradas) y vista previa perezosa de imágenes ≤ 1 MiB en el listado.
  Sin miniaturas generadas en el servidor: decodificar imágenes ajenas aquí añadiría superficie de ataque; las decodifica el navegador.
- **M3 (hecho)** `--tls`: HTTPS con certificado autofirmado de 30 días generado al arrancar (rustls + rcgen, característica
  `tls` activa por defecto; `--no-default-features` da un binario de ~0,6 MB sin TLS) y huella SHA-256 impresa.
  Entrada **«Compartir esta carpeta…»** en el gestor (menú contextual y menú principal; no en Android): solo busca
  `filebrowser-server` junto al gestor o en el PATH y lo ejecuta a la vista en el terminal integrado (consola nueva en Windows) con
  `--bind 0.0.0.0 --ttl 2h`. El gestor no enlaza código de red; sin el servidor instalado muestra un aviso.
- **Auditoría (hecha, 3 agentes a ciegas: rutas/HTTP, DoS/ZIP/TLS, HTML/cabeceras).** Sin lectura fuera de la raíz ni caídas
  en el modo de solo lectura (con `--write`, la auditoría de 2026 encontró una: ver más abajo).
  Corregido: plazo total para handshake TLS y cabeceras (antes un byte cada pocos segundos retenía el hilo), IPv6 agrupada por /64 en
  los límites, `O_NOFOLLOW` al abrir archivos y ZIP (carrera con enlaces), rechazo de marcas bidi/ancho cero en nombres, cabeceras con
  espacio antes de `:` (400), nombres reservados de Windows con espacio final y `COM¹²³`.
  **Aceptado y documentado:** lectura muy lenta con token puede retener hilos (tope de 8 por IP); el bloqueo por IP afecta a clientes tras
  un mismo NAT/túnel; un ZIP que supera los topes o con archivos ilegibles se cierra válido pero incompleto; el PDF inline no lleva `sandbox`
  (el visor del navegador lo ejecuta aislado; `?dl=1` lo fuerza a descarga); sin HSTS con `--tls`.

- **M4 (hecho)** **Interfaz web = el propio gestor compilado a WebAssembly** (`/s/<token>/app/`, `server/build-web.sh`), no una
  reimplementación. La capa `src/vfs/` del gestor sustituye `std::fs`/`std::thread` en wasm: archivos por XHR síncrono al servidor
  (listados con caché de 3 s, lecturas por rangos de 512 KiB), hilos ejecutados en línea, zona local en memoria para temporales,
  configuración en `localStorage`. Sin terminal, 3D por GPU, audio, RAR/7z ni papelera (el borrado es definitivo).
- **M5 (hecho)** `--write` (+ `--max-upload`): `PUT /s/<tok>/<ruta>` (escribe a `.fbtmp-*` y renombra; reemplaza archivos, no carpetas)
  y `POST /s/<tok>/api/op?op=mkdir|mkdirs|rm|rmdir|rmrf|mv|cp`. Cada petición que modifica exige `X-FB: 1` y `Origin` igual a `Host`
  (frente a CSRF desde otras webs), pasa por la misma jaula que la lectura, rechaza nombres ocultos, no toca nunca las raíces ni sigue
  enlaces que salgan de ellas, limita `mv`/`cp` a la misma raíz, aplica el tope de tamaño y plazo por lectura (30 s) y deja un registro
  `[escritura]` en stderr. Tests de extremo a extremo en `write.rs`.
- **M6 (hecho)** `--user` (+ `FB_PASSWORD` o `--password-file`, mínimo 8 caracteres): HTTP Basic además del token; comparación en tiempo
  constante y los fallos cuentan para el bloqueo por IP. **Aceptado:** sin `--tls` (o túnel) la contraseña viaja sin cifrar (aviso al
  arrancar); un solo usuario, sin cierre de sesión. No se permite compartir `/` entero.
- **Limitaciones aceptadas de la interfaz web:** cada consulta de metadatos es una petición síncrona (carpetas enormes van lentas);
  la búsqueda de contenido se hace en el hilo de la interfaz con tope de 3000 archivos; los duplicados y el cálculo de hashes bloquean
  la página mientras duran; las miniaturas solo se decodifican para imágenes ≤ 4 MiB; las operaciones `mv`/`cp` del servidor se limitan a una misma raíz.

- **Auditoría web (hecha, 3 agentes a ciegas: rutas y escritura, HTTP/auth/DoS, navegador y contenido).** Sin salida de las raíces,
  sin XSS, sin ejecución de contenido activo, sin saltos de autenticación ni de CSRF.
  Corregido: `PUT` sin `Content-Length` vaciaba el archivo (ahora 411); con `--tls` una conexión muda retenía un hilo para siempre (la
  respuesta de error tiene plazo); el bloqueo por IP ya no deja fuera a quien trae el enlace correcto (con contraseña, esto convertía el
  bloqueo en un oráculo: rehecho en la auditoría 2026, ver abajo);
  `mv` ya no pisa un archivo distinto que solo difiere en mayúsculas; `rmrf`/`mv` de carpetas se niegan (409) si arrastrarían contenido
  oculto; las raíces anidadas no se pueden borrar ni mover desde la exterior; `cp` respeta `--max-upload`; barrido al arrancar de los
  `.fbtmp-*` huérfanos de más de una hora; un `PUT` que reemplaza conserva los permisos; cabeceras repetidas (`Content-Length`, `Host`,
  `Origin`, `X-FB`, `Authorization`, `Range`, `If-None-Match`), `Content-Length` con signo y CR/LF sueltos se rechazan (400);
  esquema `Basic` sin distinguir mayúsculas; `Origin` debe coincidir también en esquema (http/https según `--tls`); más caracteres
  invisibles rechazados en nombres (U+061C, U+00AD, U+034F, rellenos hangul, etiquetas U+E0000–E007F); `window.open` con `noopener` y
  `Cross-Origin-Opener-Policy: same-origin`; `form-action 'none'` en la CSP de `/app/`; IPv4 mapeada en IPv6 normalizada en los límites.
  **Aceptado y documentado:** subidas y descargas muy lentas sin plazo total y sin cuota de disco (solo las puede abusar quien tiene el
  enlace); TOCTOU de un atacante local con escritura en la carpeta compartida (mitigación fuerte: `openat2`/`O_PATH`); el PDF inline
  sin `sandbox` (lo contiene el visor del navegador); `filebrowser.config` en `localStorage` se comparte por `host:puerto` (el token
  cambia en cada arranque, así que no puede ser parte de la clave sin perder la persistencia); el token vive en la URL; el mapa del
  limitador se vacía al superar 4096 IPs; el 503 por exceso de conexiones va en claro aunque haya `--tls`.

- **Auditoría 2026 (hecha).** Sin salida de la jaula, salto de autenticación, XSS ni pánico alcanzable por red. Corregido:
  - **Bloqueo por IP ineficaz con contraseña:** con la IP bloqueada se seguía comprobando la contraseña y se dejaba pasar la buena,
    así que el bloqueo no frenaba a nadie (era un oráculo). Ahora, bloqueado, la contraseña no se evalúa y la respuesta es siempre
    el mismo 429 (`Retry-After`). Solo se libra del bloqueo un navegador que ya se autenticó: recibe entonces una cookie de sesión
    aleatoria de 128 bits (`HttpOnly`, `SameSite=Lax`, `Path=/s/<tok>/`, `Secure` con `--tls`) que no sustituye a la contraseña, solo
    exime del bloqueo; si pierde las credenciales recibe 401 para volver a pedirlas. Además hay un tope global de 100 contraseñas
    fallidas por minuto sumando todas las IPs. Sin `--user`, el token (128 bits) correcto sigue entrando con la IP bloqueada.
  - **(Windows, `--write`)** los nombres cortos 8.3 (`GIT~1`, `ENV~1`) se saltaban el filtro de ocultos en `PUT`/`mv`/`rm`: si un
    nombre con `~` existe y su nombre largo real (GetFinalPathNameByHandle) es otro, 403.
  - **(`--write`)** la comprobación de ocultos antes de `rmrf`/`mv` era recursiva en la pila de 512 KiB del manejador y daba «nada
    oculto» si una carpeta no se podía leer: ahora es iterativa, con topes (1 000 000 entradas, 1024 niveles) y ante la duda niega
    (409); `remove_dir_all` corre en un hilo con pila de 64 MiB.
  - Huecos de conexión: el plazo de handshake + cabeceras baja a 5 s y, con todos los huecos ocupados, la conexión sin autenticar
    más antigua (≥ 1 s) se corta para dejar entrar a la nueva; las ya autenticadas no se tocan.
  - `mv` nunca pisa: Windows con `MoveFileEx` sin reemplazo e identidad de archivo real (antes «mismo archivo» era siempre sí);
    Linux con `renameat2(RENAME_NOREPLACE)`; otros Unix, comprobación previa por dev/ino.
  - Listados HTML/JSON y búsqueda con tope de 4 MiB (antes 20 000 filas sin tope de bytes).
  - Windows: negativa a correr elevado (salvo `--i-know`), contraseña sin eco en la consola, apertura sin seguir enlaces/uniones
    (`FILE_FLAG_OPEN_REPARSE_POINT`; los demás puntos de análisis, p. ej. OneDrive, se reabren y se exige el mismo identificador).
  - Raíces llamadas `app` o `api` (tapadas por las rutas propias) se renombran a `app-2`/`api-2`.
  - La guarda de raíces anidadas compara también la identidad de archivo (sistemas sin distinción de mayúsculas, nombres cortos).
  - Al caducar `--ttl` se borran los temporales de las subidas en curso; el barrido al arrancar solo toca nombres exactos
    `.fbtmp-` + 16 hex.
  - `FB_PASSWORD` se borra del entorno tras leerla (en Linux también se sobrescribe en el bloque que muestra `/proc/<pid>/environ`).
  - `--ttl` con un sufijo de varios bytes ya no provoca pánico; la ayuda ya no dice «No hay HTTPS».
  **Evaluado y no hecho: lista de `Host` permitidos (DNS rebinding).** Un sitio que rebinde su nombre a la IP del servidor solo
  obtiene el mismo origen que un desconocido: sin el token de la ruta todo es 404 (y con `--user`, además, 401/429), y las escrituras
  piden `X-FB` + `Origin` = `Host`, que tampoco sirven sin token. Una lista cerrada (IPs + `localhost`) rompería el acceso por nombre
  en la LAN (`pc.local`, nombres de Tailscale) sin cerrar nada que el token no cierre ya; con `--tls` un nombre ajeno además da error
  de certificado.
  **Aceptado:** con `--user` y la IP o el total bloqueados, un navegador nuevo (sin cookie) espera hasta un minuto; la cookie de sesión
  no caduca hasta 24 h o hasta reiniciar el servidor; Ctrl+C (sin `--ttl`) puede dejar `.fbtmp-*`, que barre el siguiente arranque con
  `--write` pasada una hora.

## Decisiones abiertas
- ~~`tiny_http` o servidor propio~~: resuelto, servidor propio sobre `std::net` (sin dependencias de red, ~400 líneas de HTTP/enrutado a auditar).
- ¿Android? Un servidor en el móvil implica servicio en primer plano; se deja fuera de v1.
- ¿Subida de archivos más adelante? Si sí, será otro modo explícito con su propia auditoría.
