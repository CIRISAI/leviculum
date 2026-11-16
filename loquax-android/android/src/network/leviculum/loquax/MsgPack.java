package network.leviculum.loquax;

import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The msgpack the library speaks (loquax-android/src/schema.rs), without a
 * library: maps, arrays, strings, integers, booleans, nil, and bin and
 * floats when they come. Integers come back as Long.
 */
final class MsgPack {
    private MsgPack() {}

    static byte[] pack(Object value) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        write(out, value);
        return out.toByteArray();
    }

    static Object unpack(byte[] bytes) {
        return new Reader(bytes).read();
    }

    private static void write(ByteArrayOutputStream out, Object value) {
        if (value == null) {
            out.write(0xc0);
        } else if (value instanceof Boolean) {
            out.write(((Boolean) value) ? 0xc3 : 0xc2);
        } else if (value instanceof Integer || value instanceof Long) {
            long n = ((Number) value).longValue();
            if (n >= 0 && n < 128) {
                out.write((int) n);
            } else if (n < 0 && n >= -32) {
                out.write((int) (n & 0xff));
            } else {
                out.write(0xd3);
                be(out, n, 8);
            }
        } else if (value instanceof Double || value instanceof Float) {
            // A real (1269, the photo viewer's zoom): always float 64.
            out.write(0xcb);
            be(out, Double.doubleToLongBits(((Number) value).doubleValue()), 8);
        } else if (value instanceof String) {
            byte[] utf8 = ((String) value).getBytes(StandardCharsets.UTF_8);
            int n = utf8.length;
            if (n < 32) {
                out.write(0xa0 | n);
            } else if (n < 0x100) {
                out.write(0xd9);
                be(out, n, 1);
            } else if (n < 0x10000) {
                out.write(0xda);
                be(out, n, 2);
            } else {
                out.write(0xdb);
                be(out, n, 4);
            }
            out.write(utf8, 0, n);
        } else if (value instanceof byte[]) {
            byte[] bin = (byte[]) value;
            out.write(0xc6);
            be(out, bin.length, 4);
            out.write(bin, 0, bin.length);
        } else if (value instanceof List) {
            List<?> list = (List<?>) value;
            header(out, list.size(), 0x90, 0xdc, 0xdd);
            for (Object item : list) {
                write(out, item);
            }
        } else if (value instanceof Map) {
            Map<?, ?> map = (Map<?, ?>) value;
            header(out, map.size(), 0x80, 0xde, 0xdf);
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                write(out, entry.getKey());
                write(out, entry.getValue());
            }
        } else {
            throw new IllegalArgumentException("no msgpack for " + value.getClass());
        }
    }

    private static void header(ByteArrayOutputStream out, int n, int fix, int b16, int b32) {
        if (n < 16) {
            out.write(fix | n);
        } else if (n < 0x10000) {
            out.write(b16);
            be(out, n, 2);
        } else {
            out.write(b32);
            be(out, n, 4);
        }
    }

    private static void be(ByteArrayOutputStream out, long n, int width) {
        for (int i = width - 1; i >= 0; i--) {
            out.write((int) (n >>> (8 * i)) & 0xff);
        }
    }

    private static final class Reader {
        private final byte[] bytes;
        private int at;

        Reader(byte[] bytes) {
            this.bytes = bytes;
        }

        private int u8() {
            if (at >= bytes.length) {
                throw new IllegalArgumentException("msgpack cut short");
            }
            return bytes[at++] & 0xff;
        }

        private long be(int width) {
            long n = 0;
            for (int i = 0; i < width; i++) {
                n = (n << 8) | u8();
            }
            return n;
        }

        private byte[] take(long n) {
            if (n < 0 || at + n > bytes.length) {
                throw new IllegalArgumentException("msgpack cut short");
            }
            byte[] out = new byte[(int) n];
            System.arraycopy(bytes, at, out, 0, (int) n);
            at += (int) n;
            return out;
        }

        Object read() {
            int b = u8();
            if (b <= 0x7f) return (long) b;
            if (b >= 0xe0) return (long) (byte) b;
            if ((b & 0xf0) == 0x80) return map(b & 0x0f);
            if ((b & 0xf0) == 0x90) return array(b & 0x0f);
            if ((b & 0xe0) == 0xa0) return str(b & 0x1f);
            switch (b) {
                case 0xc0: return null;
                case 0xc2: return Boolean.FALSE;
                case 0xc3: return Boolean.TRUE;
                case 0xc4: return take(be(1));
                case 0xc5: return take(be(2));
                case 0xc6: return take(be(4));
                case 0xca: return (double) Float.intBitsToFloat((int) be(4));
                case 0xcb: return Double.longBitsToDouble(be(8));
                case 0xcc: return be(1);
                case 0xcd: return be(2);
                case 0xce: return be(4);
                case 0xcf: return be(8);
                case 0xd0: return (long) (byte) be(1);
                case 0xd1: return (long) (short) be(2);
                case 0xd2: return (long) (int) be(4);
                case 0xd3: return be(8);
                case 0xd9: return str(be(1));
                case 0xda: return str(be(2));
                case 0xdb: return str(be(4));
                case 0xdc: return array(be(2));
                case 0xdd: return array(be(4));
                case 0xde: return map(be(2));
                case 0xdf: return map(be(4));
                default: throw new IllegalArgumentException("msgpack type " + b);
            }
        }

        private String str(long n) {
            return new String(take(n), StandardCharsets.UTF_8);
        }

        private List<Object> array(long n) {
            List<Object> list = new ArrayList<>();
            for (long i = 0; i < n; i++) {
                list.add(read());
            }
            return list;
        }

        private Map<String, Object> map(long n) {
            Map<String, Object> map = new LinkedHashMap<>();
            for (long i = 0; i < n; i++) {
                map.put(String.valueOf(read()), read());
            }
            return map;
        }
    }
}
