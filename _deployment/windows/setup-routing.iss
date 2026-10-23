// SPDX-License-Identifier: GPL-3.0-or-later

procedure SkipJsonWhitespace(const Text: String; var Index: Integer);
begin
  while Index <= Length(Text) do
  begin
    if Pos(Text[Index], ' ' + #9 + #10 + #13) = 0 then
      Break;
    Index := Index + 1;
  end;
end;

function ReadJsonString(const Text: String; var Index: Integer): String;
var
  Ch: Char;
  Hex: String;
  I: Integer;
begin
  Result := '';
  if (Index > Length(Text)) or (Text[Index] <> '"') then
    RaiseException('Invalid manifest JSON string.');
  Index := Index + 1;
  while Index <= Length(Text) do
  begin
    Ch := Text[Index];
    Index := Index + 1;
    if Ch = '"' then
      Exit;
    if Ch = '\' then
    begin
      if Index > Length(Text) then
        Break;
      Ch := Text[Index];
      Index := Index + 1;
      case Ch of
        '"', '\', '/': Result := Result + Ch;
        'b': Result := Result + #8;
        'f': Result := Result + #12;
        'n': Result := Result + #10;
        'r': Result := Result + #13;
        't': Result := Result + #9;
        'u':
          begin
            Hex := Copy(Text, Index, 4);
            if Length(Hex) <> 4 then
              RaiseException('Invalid manifest JSON escape.');
            for I := 1 to 4 do
              if Pos(UpperCase(Hex[I]), '0123456789ABCDEF') = 0 then
                RaiseException('Invalid manifest JSON escape.');
            Result := Result + Chr(StrToInt('$' + Hex));
            Index := Index + 4;
          end;
      else
        RaiseException('Invalid manifest JSON escape.');
      end;
    end
    else
    begin
      if Ord(Ch) < 32 then
        RaiseException('Invalid manifest JSON string.');
      Result := Result + Ch;
    end;
  end;
  RaiseException('Unterminated manifest JSON string.');
end;

function ReadManifestSource(const Text: String): String;
var
  Index, NextIndex, Depth: Integer;
  Value: String;
begin
  Result := '';
  Index := 1;
  Depth := 0;
  SkipJsonWhitespace(Text, Index);
  if (Index > Length(Text)) or (Text[Index] <> '{') then
    RaiseException('Invalid download manifest.');
  while Index <= Length(Text) do
  begin
    case Text[Index] of
      '{', '[': Depth := Depth + 1;
      '}', ']':
        begin
          Depth := Depth - 1;
          if Depth = 0 then
          begin
            Index := Index + 1;
            SkipJsonWhitespace(Text, Index);
            if Index <= Length(Text) then
              RaiseException('Invalid download manifest.');
            Break;
          end;
        end;
      '"':
        begin
          Value := ReadJsonString(Text, Index);
          NextIndex := Index;
          SkipJsonWhitespace(Text, NextIndex);
          if (Depth = 1) and (Value = 'selected_source') and
             (Copy(Text, NextIndex, 1) = ':') then
          begin
            if Result <> '' then
              RaiseException('Duplicate manifest download source.');
            Index := NextIndex + 1;
            SkipJsonWhitespace(Text, Index);
            Result := ReadJsonString(Text, Index);
            if (Result <> 'cn') and (Result <> 'global') then
              RaiseException('Unsupported manifest download source.');
          end;
          Continue;
        end;
    end;
    Index := Index + 1;
  end;
  if (Depth <> 0) or (Result = '') then
    RaiseException('Missing manifest download source.');
end;

function IsNightlyPackageUrl(const Url: String): Boolean;
var
  Value: String;
  Index: Integer;
begin
  Value := LowerCase(Url);
  Index := Pos('?', Value);
  if Index > 0 then
    Value := Copy(Value, 1, Index - 1);
  Result := (Pos('https://nightly.link/', Value) = 1) or
    (Pos('http://nightly.link/', Value) = 1);
  for Index := Length(Value) downto 1 do
    if Value[Index] = '/' then
    begin
      Value := Copy(Value, Index + 1, Length(Value));
      Break;
    end;
  Result := Result or (Value = 'gyroflow-niyien-windows.zip');
end;
