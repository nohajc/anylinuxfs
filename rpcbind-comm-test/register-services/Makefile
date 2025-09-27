CC = gcc
CFLAGS =
LDFLAGS = -F/System/Library/PrivateFrameworks
LIBS = -framework oncrpc

TARGET = regsvc
SRC = main.c

all: $(TARGET)

$(TARGET): $(SRC)
	$(CC) $(CFLAGS) $(LDFLAGS) -o $(TARGET) $(SRC) $(LIBS)

clean:
	rm -f $(TARGET)
